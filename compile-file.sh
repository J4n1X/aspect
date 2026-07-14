#!/bin/bash

prog_name="${1%.*}"
cargo run -- compile $1 -I lib | llc-19 -o "$prog_name.s" -x86-asm-syntax=intel
gcc -o "$prog_name.out" "$prog_name.s"