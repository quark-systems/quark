//! Files on any runtime, through [`Exec`] alone.
//!
//! Paths and data are passed as arguments and stdin, never spliced into a
//! script.

use std::path::Path;

use quark_core::{CoreError, Result};

use crate::exec::{Cmd, Exec};

fn arg(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn absolute(p: &Path) -> Result<()> {
    if p.is_absolute() {
        Ok(())
    } else {
        Err(CoreError::Invalid(format!(
            "{} is not an absolute path",
            p.display()
        )))
    }
}

/// Write `bytes` to `path`, creating its directory. Readers see the old
/// file or the new one, never a partial write.
pub async fn write(exec: &dyn Exec, path: &Path, bytes: &[u8]) -> Result<()> {
    absolute(path)?;
    let script = r#"set -e
mkdir -p "$(dirname "$1")"
tmp="$1.quark-tmp.$$"
cat > "$tmp"
mv -f "$tmp" "$1""#;
    exec.run(&Cmd::sh(script, [arg(path)]).stdin(bytes.to_vec()))
        .await?
        .check(&format!("writing {}", path.display()))?;
    Ok(())
}

/// `mkdir -p` each of `dirs`.
pub async fn mkdir_all(exec: &dyn Exec, dirs: &[&Path]) -> Result<()> {
    for d in dirs {
        absolute(d)?;
    }
    let mut argv = vec!["mkdir".to_string(), "-p".into(), "--".into()];
    argv.extend(dirs.iter().map(|d| arg(d)));
    exec.run(&Cmd::new(argv)).await?.check("mkdir")?;
    Ok(())
}

/// The whole file, or `None` when it does not exist.
pub async fn read(exec: &dyn Exec, path: &Path) -> Result<Option<Vec<u8>>> {
    read_from(exec, path, 0, u64::MAX).await
}

/// Up to `max` bytes of `path` starting at byte `offset`, or `None` when
/// the file does not exist.
pub async fn read_from(
    exec: &dyn Exec,
    path: &Path,
    offset: u64,
    max: u64,
) -> Result<Option<Vec<u8>>> {
    absolute(path)?;
    let script = r#"[ -f "$1" ] || exit 3
if [ "$3" = all ]; then tail -c "+$(($2 + 1))" "$1"
else tail -c "+$(($2 + 1))" "$1" | head -c "$3"; fi"#;
    let max = if max == u64::MAX {
        "all".to_string()
    } else {
        max.to_string()
    };
    let out = exec
        .run(&Cmd::sh(script, [arg(path), offset.to_string(), max]))
        .await?;
    if out.code == Some(3) {
        return Ok(None);
    }
    Ok(Some(
        out.check(&format!("reading {}", path.display()))?.stdout,
    ))
}

/// The names in directory `dir`, sorted; empty when it does not exist.
pub async fn list(exec: &dyn Exec, dir: &Path) -> Result<Vec<String>> {
    absolute(dir)?;
    let out = exec
        .run(&Cmd::sh(
            r#"[ -d "$1" ] || exit 0; cd "$1" && for f in * .*; do [ -e "$f" ] && [ "$f" != . ] && [ "$f" != .. ] && printf '%s\0' "$f"; done; exit 0"#,
            [arg(dir)],
        ))
        .await?
        .check(&format!("listing {}", dir.display()))?;
    let mut names: Vec<String> = out
        .stdout
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    names.sort();
    Ok(names)
}

/// Whether anything exists at `path`.
pub async fn exists(exec: &dyn Exec, path: &Path) -> Result<bool> {
    absolute(path)?;
    let out = exec
        .run(&Cmd::sh(r#"[ -e "$1" ] || [ -L "$1" ]"#, [arg(path)]))
        .await?;
    Ok(out.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalRuntime;
    use quark_core::HostId;

    #[tokio::test]
    async fn write_read_list() {
        let rt = LocalRuntime::new(HostId::from("h"));
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a b/c's.txt");
        assert_eq!(read(&rt, &f).await.unwrap(), None);
        write(&rt, &f, b"hello\nworld\n").await.unwrap();
        assert_eq!(read(&rt, &f).await.unwrap().unwrap(), b"hello\nworld\n");
        assert_eq!(
            read_from(&rt, &f, 6, 3).await.unwrap().unwrap(),
            b"wor".to_vec()
        );
        assert_eq!(read_from(&rt, &f, 12, 9).await.unwrap().unwrap(), b"");
        write(&rt, &dir.path().join("a b/.hidden"), b"")
            .await
            .unwrap();
        assert_eq!(
            list(&rt, &dir.path().join("a b")).await.unwrap(),
            [".hidden", "c's.txt"]
        );
        assert!(list(&rt, &dir.path().join("none"))
            .await
            .unwrap()
            .is_empty());
        assert!(exists(&rt, &f).await.unwrap());
        mkdir_all(&rt, &[&dir.path().join("x/y")]).await.unwrap();
        assert!(dir.path().join("x/y").is_dir());
        assert!(write(&rt, Path::new("rel"), b"").await.is_err());
    }
}
