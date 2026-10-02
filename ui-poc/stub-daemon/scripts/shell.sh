#!/usr/bin/env bash
# Interactive shell for input-latency tests: typed input is echoed by the tty.
export LANG=C.UTF-8 LC_ALL=C.UTF-8
export PS1='\[\e[1;32m\]worker@quark\[\e[0m\]:\[\e[1;34m\]\w\[\e[0m\]\$ '
cd "${HOME:-/}" || true
exec bash --noprofile --norc -i
