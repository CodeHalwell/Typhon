#!/usr/bin/env bash
# Stress harness for the 2026-06-21 "valid runnable Python" sweep.
#
# For each .ty argument, run three paths and compare:
#   1. tyc check            — frontend (parser + resolver + type checker)
#   2. tyc build + python   — the PRODUCTION path (this is what "compiles to
#                             valid, runnable Python" actually means)
#   3. tyc run              — in-process tree-walking VM
#
# The primary verdict is the build+python path. VM divergence is reported as
# secondary info (the VM intentionally does not cover every surface — e.g.
# native async, unbounded generators).
#
# Overridable: TYC=/path/to/tyc  PYTHON=python3.13
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
TYC=${TYC:-"$REPO_ROOT/tyc/target/release/tyc"}
PY=${PYTHON:-python3.13}
PROJ="$SCRIPT_DIR/proj"

pass=0; buildfail=0; runfail=0; checkfail=0; diverge=0; total=0
mkdir -p "$PROJ/src"
cat > "$PROJ/typhon.toml" <<'TOML'
[project]
name = "stressproj"
version = "0.1.0"
src = "src"
out = "build"
[python]
target = "3.13"
[emit]
format = false
[strictness]
unused-import = "warn"
TOML

for f in "$@"; do
  name=$(basename "$f" .ty)
  total=$((total+1))

  chk=$("$TYC" check "$f" 2>&1); chk_rc=$?
  vm=$(TYC_SKIP_CHECK=1 "$TYC" run "$f" 2>&1); vm_rc=$?

  cp "$f" "$PROJ/src/main.ty"
  rm -rf "$PROJ/build"
  if (cd "$PROJ" && "$TYC" build >/tmp/bld.log 2>&1); then
    comp=$("$PY" "$PROJ/build/main.py" 2>&1); crc=$?
  else
    comp="BUILD_FAIL"; crc=99
    bld_err=$(grep -iE 'error|panic' /tmp/bld.log | head -4)
  fi

  # Verdict on the production path.
  if   [ "$crc" == "99" ]; then status="BUILDFAIL"; buildfail=$((buildfail+1))
  elif [ "$crc" != "0" ];  then status="RUNFAIL";   runfail=$((runfail+1))
  else status="PASS"; pass=$((pass+1)); fi
  [ "$chk_rc" != "0" ] && checkfail=$((checkfail+1))

  vmnote=""
  if [ "$status" == "PASS" ] && { [ "$vm" != "$comp" ] || [ "$vm_rc" != "$crc" ]; }; then
    vmnote=" {VM-DIVERGE}"; diverge=$((diverge+1))
  fi

  echo "===== $name (check=$chk_rc vm=$vm_rc comp=$crc) [$status]$vmnote ====="
  if [ "$status" != "PASS" ] || [ "$chk_rc" != "0" ] || [ -n "$vmnote" ]; then
    if [ "$chk_rc" != "0" ]; then echo "--- check(tail) ---"; echo "$chk" | tail -6; fi
    if [ "$status" == "BUILDFAIL" ]; then echo "--- build err ---"; echo "$bld_err"; fi
    if [ "$status" == "RUNFAIL" ];   then echo "--- python err ---"; echo "$comp" | tail -8; fi
    if [ -n "$vmnote" ]; then
      echo "--- VM ---";          echo "$vm"   | head -12
      echo "--- BUILD+PY ---";    echo "$comp" | head -12
    fi
  fi
done

echo
echo "######## SUMMARY ########"
echo "total=$total pass=$pass buildfail=$buildfail runfail=$runfail checkfail=$checkfail vm_diverge=$diverge"
