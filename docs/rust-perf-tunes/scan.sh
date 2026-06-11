#!/usr/bin/env bash
# Written in [Amber](https://amber-lang.com/)
# version: 0.6.0-alpha
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
# split(text: Text, delimiter: Text)
split__4_v0() {
    local text_12="${1}"
    local delimiter_13="${2}"
    local result_14=()
    # zsh uses -A for array, bash uses -a, ksh is VERY bad at splitting anything
    if [ "$([ "_${EXEC_SHELL}" != "_zsh" ]; echo $?)" != 0 ]; then
        IFS="${delimiter_13}" read -rd '' -A result_14 < <(printf %s "$text_12")
    elif [ "$([ "_${EXEC_SHELL}" != "_ksh" ]; echo $?)" != 0 ]; then
        if [ "$([ "_${delimiter_13}" != "_
" ]; echo $?)" != 0 ]; then
            while read -r -d $'\n'; do result_14+=("$REPLY"); done < <(echo "$text_12")
        else
            IFS="${delimiter_13}" read -rd '' -a result_14 < <(printf %s "$text_12")
        fi
    elif [ "$([ "_${EXEC_SHELL}" != "_bash" ]; echo $?)" != 0 ]; then
        IFS="${delimiter_13}" read -rd '' -a result_14 < <(printf %s "$text_12")
    fi
    ret_split4_v0=("${result_14[@]}")
    return 0
}

# trim(text: Text)
trim__10_v0() {
    local text_18="${1}"
    local result_19=""
    result_19="${text_18#${text_18%%[![:space:]]*}}"
    result_19="${result_19%${result_19##*[![:space:]]}}"
    ret_trim10_v0="${result_19}"
    return 0
}

# crabcc Rust performance-tunes scanner — Pass 1 of the two-pass pipeline.
# 
# Proper AST matching: each tune's `ast_grep_pattern` is matched through
# ast-grep (tree-sitter), so occurrences in comments/strings are ignored —
# unlike the `anti_pattern_regex` fallback. Matched tunes (full rules) plus the
# target file are written to refactor-context.json for Pass 2 (the refactor
# agent) to apply, avoiding context dilution from dumping the whole dataset.
# 
# Modern CLI tooling: ast-grep (AST) + jq (dataset), orchestrated in Amber
# (typed bash) → compiles to portable POSIX bash via `amber build scan.ab scan.sh`.
# 
# Usage:  amber run scan.ab <tunes.json> <target-path>
# or:   ./scan.sh <tunes.json> <target-path>
# Run from this directory so the *.jq files resolve, or pass absolute paths.
typeset -r args_3=("$0" "$@")
__length_2=("${args_3[@]}")
if [ "$(( ${#__length_2[@]} < 3 ))" != 0 ]; then
    echo "usage: scan <tunes.json> <target-path>"
    exit 1
fi
tunes_4="${args_3[1]?"Index out of bounds (at scan.ab:23:24)"}"
target_5="${args_3[2]?"Index out of bounds (at scan.ab:24:25)"}"
# Pull "id<TAB>pattern" for every tune that defines an ast-grep pattern.
rows_6="$(jq -rf tune_rows.jq "${tunes_4}")"
matched_7=""
hits_8=0
split__4_v0 "${rows_6}" "
"
ret_split4_v0__31_16=("${ret_split4_v0[@]}")
for row_15 in "${ret_split4_v0__31_16[@]}"; do
    trim__10_v0 "${row_15}"
    line_20="${ret_trim10_v0}"
    if [ "$([ "_${line_20}" == "_" ]; echo $?)" != 0 ]; then
        split__4_v0 "${line_20}" "	"
        parts_21=("${ret_split4_v0[@]}")
        id_22="${parts_21[0]?"Index out of bounds (at scan.ab:35:30)"}"
        pattern_23="${parts_21[1]?"Index out of bounds (at scan.ab:36:35)"}"
        # ast-grep returns a JSON array of matches; jq length counts them.
        command_6="$(ast-grep run --pattern "${pattern_23}" --lang rust "${target_5}" --json=compact 2>/dev/null | jq 'length')"
        trim__10_v0 "${command_6}"
        count_24="${ret_trim10_v0}"
        if [ "$(( $([ "_${count_24}" == "_0" ]; echo $?) && $([ "_${count_24}" == "_" ]; echo $?) ))" != 0 ]; then
            echo "  ⚠ ${id_22}: ${count_24} site(s) — pattern: ${pattern_23}"
            matched_7="${matched_7}${id_22} "
            hits_8="$(( hits_8 + 1 ))"
        fi
    fi
done
if [ "$(( hits_8 == 0 ))" != 0 ]; then
    echo "✓ no tune triggers matched in ${target_5}"
    exit 0
fi
# Pass-2 bundle: matched tunes + the file, for the refactor agent.
# No `silent` here — it would append its own >/dev/null after our redirect.
trim__10_v0 "${matched_7}"
ids_25="${ret_trim10_v0}"
jq -f refactor_context.jq --arg ids "${ids_25}" --arg file "${target_5}" "${tunes_4}" > refactor-context.json
printf '%s\n' ""
echo "🤖 ${hits_8} optimization vector(s) — wrote refactor-context.json for the refactor agent."
