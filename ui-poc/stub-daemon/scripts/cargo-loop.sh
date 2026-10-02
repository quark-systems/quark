#!/usr/bin/env bash
# Replays a colourful cargo build + test log forever, at a human-ish pace.
G=$'\e[1;32m'; R=$'\e[1;31m'; Y=$'\e[1;33m'; B=$'\e[1;34m'; C=$'\e[36m'; D=$'\e[2m'; N=$'\e[0m'
crates=(proc-macro2 unicode-ident quote syn serde_derive serde libc memchr bytes pin-project-lite
  tokio-macros mio socket2 tokio itoa ryu serde_json tracing-core tracing http httparse
  base64 axum-core tower-layer tower-service tower matchit axum quark-proto quark-store quarkd)
tests=(store::append_assigns_seq store::evicts_oldest_worker_output events::lagged_subscriber_resyncs
  events::publish_reaches_subscriber tmux::decodes_octal_escapes tmux::hex_input_roundtrip
  api::answer_rejects_out_of_range api::answer_is_final chat::deltas_concatenate_to_final
  workers::resize_rejects_tiny_panes workers::stress_toggle_is_idempotent)
run=0
while true; do
  run=$((run + 1))
  printf '%s$%s cargo test --workspace %s# run %d%s\n' "$C" "$N" "$D" "$run" "$N"
  sleep 0.6
  for c in "${crates[@]}"; do
    printf '%s   Compiling%s %s v%d.%d.%d\n' "$G" "$N" "$c" $((RANDOM % 2)) $((RANDOM % 40)) $((RANDOM % 12))
    sleep "0.0$((RANDOM % 9 + 1))"
  done
  if (( run % 3 == 0 )); then
    printf '%swarning%s%s: unused variable: `retained`%s\n' "$Y" "$N" $'\e[1m' "$N"
    printf '  %s-->%s crates/quarkd/src/store.rs:142:13\n' "$B" "$N"
    printf '   %s|%s\n%s142%s %s|%s         let retained = self.bytes_for(worker);\n' "$B" "$N" "$B" "$N" "$B" "$N"
    printf '   %s|%s             %s^^^^^^^^ help: prefix it with an underscore: `_retained`%s\n\n' "$B" "$N" "$Y" "$N"
  fi
  printf '%s    Finished%s `test` profile [unoptimized + debuginfo] target(s) in %d.%02ds\n' "$G" "$N" $((RANDOM % 20 + 3)) $((RANDOM % 100))
  printf '%s     Running%s unittests src/lib.rs (target/debug/deps/quarkd-%04x%04x)\n\n' "$G" "$N" $RANDOM $RANDOM
  printf 'running %d tests\n' "${#tests[@]}"
  failed=""
  for t in "${tests[@]}"; do
    sleep "0.$((RANDOM % 3 + 1))"
    if (( run % 4 == 0 )) && [[ $t == chat::deltas_concatenate_to_final ]]; then
      printf 'test %s ... %sFAILED%s\n' "$t" "$R" "$N"; failed=$t
    else
      printf 'test %s ... %sok%s\n' "$t" "$G" "$N"
    fi
  done
  echo
  if [[ -n $failed ]]; then
    printf 'failures:\n\n---- %s stdout ----\n' "$failed"
    printf "thread '%s' panicked at crates/quarkd/src/chat.rs:88:9:\nassertion \`left == right\` failed\n  left: \"Here is the plan\"\n right: \"Here is the plan.\"\n\n" "$failed"
    printf 'test result: %sFAILED%s. %d passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.%02ds\n\n' "$R" "$N" $(( ${#tests[@]} - 1 )) $((RANDOM % 100))
    printf '%serror%s: test failed, to rerun pass `-p quarkd --lib`\n\n' "$R" "$N"
  else
    printf 'test result: %sok%s. %d passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.%02ds\n\n' "$G" "$N" "${#tests[@]}" $((RANDOM % 100))
  fi
  sleep 3
done
