#!/usr/bin/env bash
# Written in [Amber](https://amber-lang.com/)
# version: 0.6.0-alpha
pids_0=()
for (( ____1=0; ____1 < 10; ____1++ )); do
    sleep 0.1 &
    __status=$?
    __pid_3=$!
    array_2=("${__pid_3}")
    pids_0+=("${array_2[@]}")
done
wait "${pids_0[@]}"
