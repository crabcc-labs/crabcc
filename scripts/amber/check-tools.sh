#!/usr/bin/env bash
# Written in [Amber](https://amber-lang.com/)
# version: 0.6.0-alpha
[ "$EUID" -ne 0 ] && { { command -v sudo >/dev/null 2>&1 && __sudo=sudo; } || { command -v doas >/dev/null 2>&1 && __sudo=doas; }; }
if [ -n "$ZSH_VERSION" ]; then
    EXEC_SHELL="zsh"
    IFS='.' read -A EXEC_SHELL_VERSION <<< "$ZSH_VERSION"
elif [ -n "$KSH_VERSION" ]; then
    EXEC_SHELL="ksh"
    __exec_shell_version="${.sh.version##*/}"
    IFS='.' read -a EXEC_SHELL_VERSION <<< "${__exec_shell_version%% *}"
else
    EXEC_SHELL="bash"
    EXEC_SHELL_VERSION=("${BASH_VERSINFO[0]}" "${BASH_VERSINFO[1]}" "${BASH_VERSINFO[2]}")
fi
# printf(format: Text, args: [Text])
printf__128_v0() {
    local format_18="${1}"
    local args_19=("${!2}")
    args_19=("${format_18}" "${args_19[@]}")
    printf "${args_19[@]}"
}

# echo_success(message: Text)
echo_success__136_v0() {
    local message_17="${1}"
    local array_0=("${message_17}")
    printf__128_v0 "\\x1b[1;3;97;42m%s\\x1b[0m
" array_0[@]
}

# echo_warning(message: Text)
echo_warning__137_v0() {
    local message_21="${1}"
    local array_1=("${message_21}")
    printf__128_v0 "\\x1b[1;3;97;43m%s\\x1b[0m
" array_1[@]
}

__REQUIRED_3=("cargo" "rustup" "git" "jq" "node" "python3" "docker" "task" "amber" "repomix")
# probe_bg(name: Text)
probe_bg__199_v0() {
    local name_8="${1}"
    command -v "${name_8}" > /dev/null 2>&1 &
    ret_probe_bg199_v0=$!
    return 0
}

pids_4=()
tools_5=()
for name_6 in "${__REQUIRED_3[@]}"; do
    probe_bg__199_v0 "${name_6}"
    ret_probe_bg199_v0__32_19="${ret_probe_bg199_v0}"
    pids_4+=("${ret_probe_bg199_v0__32_19}")
    tools_5+=("${name_6}")
done
wait "${pids_4[@]}"
__status=$?
ok_9=0
missing_10=()
for name_11 in "${tools_5[@]}"; do
    command -v "${name_11}" > /dev/null 2>&1
    __status=$?
    if [ "${__status}" != 0 ]; then
        missing_10+=("${name_11}")
        continue
    fi
    ok_9="$(( ok_9 + 1 ))"
done
__length_15=("${__REQUIRED_3[@]}")
slice_upper_16="${ok_9}"
slice_offset_17=0
slice_offset_17=$((${slice_offset_17} > 0 ? ${slice_offset_17} : 0))
slice_length_18="$(( slice_upper_16 - slice_offset_17 ))"
slice_length_18=$((${slice_length_18} > 0 ? ${slice_length_18} : 0))
echo_success__136_v0 "  ok  [${ok_9}/${#__length_15[@]}]: ${tools_5[@]:${slice_offset_17}:${slice_length_18}}"
__length_19=("${missing_10[@]}")
if [ "$(( ${#__length_19[@]} > 0 ))" != 0 ]; then
    echo_warning__137_v0 "  missing: ${missing_10[@]}"
else
    echo_success__136_v0 "all tools present"
fi
