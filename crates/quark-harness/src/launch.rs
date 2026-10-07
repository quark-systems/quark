//! Building a harness's launch command from its manifest.

use quark_core::harness::HarnessManifest;

/// Values for the launch placeholders. An unset value drops every argument
/// and optional flag that names it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchVars {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub prompt: Option<String>,
    pub prompt_file: Option<String>,
    pub cwd: Option<String>,
}

impl LaunchVars {
    fn get(&self, name: &str) -> Option<&str> {
        match name {
            "model" => self.model.as_deref(),
            "effort" => self.effort.as_deref(),
            "prompt" => self.prompt.as_deref(),
            "prompt_file" => self.prompt_file.as_deref(),
            "cwd" => self.cwd.as_deref(),
            _ => None,
        }
    }

    /// `arg` with every `{name}` replaced, or `None` when a placeholder has
    /// no value. Unknown placeholders count as unset.
    fn fill(&self, arg: &str) -> Option<String> {
        let mut out = String::with_capacity(arg.len());
        let mut rest = arg;
        while let Some(start) = rest.find('{') {
            let Some(len) = rest[start..].find('}') else {
                break;
            };
            out.push_str(&rest[..start]);
            out.push_str(self.get(&rest[start + 1..start + len])?);
            rest = &rest[start + len + 1..];
        }
        out.push_str(rest);
        Some(out)
    }
}

fn holds_prompt(arg: &str) -> bool {
    arg.contains("{prompt}") || arg.contains("{prompt_file}")
}

/// The argv for `m`: `launch.argv` with placeholders filled, and each
/// `launch.optional` group whose placeholders all have values inserted
/// before the first prompt argument, or at the end.
pub fn argv(m: &HarnessManifest, vars: &LaunchVars) -> Vec<String> {
    let split = m
        .launch
        .argv
        .iter()
        .position(|a| holds_prompt(a))
        .unwrap_or(m.launch.argv.len());
    let (head, tail) = m.launch.argv.split_at(split);
    let optional = m.launch.optional.iter().filter_map(|group| {
        group
            .iter()
            .map(|a| vars.fill(a))
            .collect::<Option<Vec<_>>>()
    });
    head.iter()
        .filter_map(|a| vars.fill(a))
        .chain(optional.flatten())
        .chain(tail.iter().filter_map(|a| vars.fill(a)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ManifestRegistry;

    fn vars(model: Option<&str>, effort: Option<&str>) -> LaunchVars {
        LaunchVars {
            model: model.map(Into::into),
            effort: effort.map(Into::into),
            prompt: Some("do it".into()),
            prompt_file: None,
            cwd: Some("/w/t1".into()),
        }
    }

    #[test]
    fn optional_flags_sit_before_the_prompt() {
        let reg = ManifestRegistry::builtin();
        assert_eq!(
            argv(
                reg.get("codex").unwrap(),
                &vars(Some("gpt-5.6"), Some("high"))
            ),
            [
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "--model",
                "gpt-5.6",
                "-c",
                "model_reasoning_effort=\"high\"",
                "do it"
            ]
        );
        assert_eq!(
            argv(reg.get("cursor-agent").unwrap(), &vars(None, None)),
            [
                "cursor-agent",
                "--trust",
                "--yolo",
                "--workspace",
                "/w/t1",
                "do it"
            ]
        );
    }

    #[test]
    fn pasted_prompts_append_flags_and_drop_unset_values() {
        let reg = ManifestRegistry::builtin();
        assert_eq!(
            argv(reg.get("kiro").unwrap(), &vars(None, Some("max"))),
            [
                "kiro-cli",
                "chat",
                "--v3",
                "--trust-all-tools",
                "--effort",
                "max"
            ]
        );
        let mut v = vars(None, None);
        v.prompt = None;
        assert_eq!(argv(reg.get("pi").unwrap(), &v), ["pi"]);
    }

    #[test]
    fn fills_placeholders_inside_arguments() {
        let v = vars(Some("m"), None);
        assert_eq!(v.fill("a={model}!").as_deref(), Some("a=m!"));
        assert_eq!(v.fill("{effort}"), None);
        assert_eq!(v.fill("{unknown}"), None);
        assert_eq!(v.fill("plain {").as_deref(), Some("plain {"));
    }
}
