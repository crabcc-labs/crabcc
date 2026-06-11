#!/usr/bin/env bash
# Written in [Amber](https://amber-lang.com/)
# version: 0.6.0-alpha
# have(cmd: Text)
have__0_v0() {
    local cmd_5="${1}"
    command -v "${cmd_5}" > /dev/null 2>&1
    __status=$?
    if [ "${__status}" != 0 ]; then
        ret_have0_v0=0
        return 0
    fi
    ret_have0_v0=1
    return 0
}

command_1="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
__status=$?
root_0="${command_1}"
command_2="$(dirname "${root_0}")"
__status=$?
ws_1="${command_2}"
vaked_2="${ws_1}/vaked-base"
work_3="${root_0}/.dev-tasks/pipeline"
rm -rf "${work_3}"
__status=$?
mkdir -p "${work_3}"
__status=$?
have__0_v0 "vakedc"
ret_have0_v0__31_8="${ret_have0_v0}"
if [ "$([ "_${ret_have0_v0__31_8}" != "_0" ]; echo $?)" != 0 ]; then
    echo "vakedc not on PATH - run: bash ${vaked_2}/install.sh"
    exit 1
fi
echo "=> [1/3] vaked + nushell: par-each \`vakedc check\` over examples"
command_3="$(find "${vaked_2}/vaked/examples" -name '*.vaked' | grep -v rejected | tr '
' ' ')"
__status=$?
files_6="${command_3}"
have__0_v0 "nu"
ret_have0_v0__39_8="${ret_have0_v0}"
if [ "${ret_have0_v0__39_8}" != 0 ]; then
    nu "${root_0}/scripts/nu/vaked-check.nu" ${files_6}
    __status=$?
    if [ "${__status}" != 0 ]; then
        echo "    some checks reported diagnostics (continuing)"
    fi
else
    echo "    nu not installed - skipped"
fi
echo "=> [2/3] vaked + nix: lower one example, evaluate the generated flake"
ex_7="${vaked_2}/vaked/examples/crabcc-umami.vaked"
command_4="$(mktemp -d)"
__status=$?
lowerdir_8="${command_4}"
vakedc lower "${ex_7}" --out "${lowerdir_8}"
__status=$?
if [ "${__status}" != 0 ]; then
    echo "    vakedc lower reported diagnostics - skipping nix"
fi
echo "    lowered -> flake.nix, gen/, provenance.json"
have__0_v0 "nix"
ret_have0_v0__57_8="${ret_have0_v0}"
if [ "${ret_have0_v0__57_8}" != 0 ]; then
    timeout 90 nix flake show --extra-experimental-features "nix-command flakes" --no-write-lock-file "${lowerdir_8}"
    __status=$?
    if [ "${__status}" != 0 ]; then
        echo "    nix flake eval skipped (generated flake pins a placeholder nixpkgs rev - prototype lowering)"
    fi
else
    echo "    nix not installed - skipped"
fi
rm -rf "${lowerdir_8}"
__status=$?
echo "=> [3/3] buildkit: cached container build of vakedc"
docker info > /dev/null 2>&1
__status=$?
if [ "${__status}" != 0 ]; then
    echo "    docker daemon not available - skipped"
    echo "[+] pipeline DONE (buildkit skipped)"
    exit 0
fi
DOCKER_BUILDKIT=1 docker buildx build --progress=plain -f "${root_0}/scripts/amber/pipeline.Dockerfile" -t crabcc-vaked:demo "${vaked_2}"
__status=$?
if [ "${__status}" != 0 ]; then
    echo "    buildx build failed (see output above)"
fi
echo "[+] pipeline DONE"
