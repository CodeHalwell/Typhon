#!/usr/bin/env bash
# For each .ty: tyc check; VM (tyc run); tyc build + python3.13. Compare VM vs CPython.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
export PATH="$REPO_ROOT/tyc/target/release:$PATH"
PROJ="$SCRIPT_DIR/proj"
PY=python3.13
for f in "$@"; do
  name=$(basename "$f" .ty)
  chk=$(tyc check "$f" 2>&1); chk_rc=$?
  vm=$(TYC_SKIP_CHECK=1 tyc run "$f" 2>&1); vm_rc=$?
  mkdir -p "$PROJ/src"
  cp "$f" "$PROJ/src/main.ty"
  rm -rf "$PROJ/build"
  if (cd "$PROJ" && tyc build >/tmp/bld.log 2>&1); then
    comp=$($PY "$PROJ/build/main.py" 2>&1); crc=$?
  else
    comp="BUILD_FAIL: $(grep -A3 -i error /tmp/bld.log | head -5)"; crc=99
  fi
  status="OK"
  # Diverge on either output OR exit-status mismatch (same output with
  # different exit codes is still a silent divergence).
  if [ "$vm" != "$comp" ] || [ "$vm_rc" != "$crc" ]; then status="DIVERGE"; fi
  if [ "$crc" == "99" ]; then status="BUILDFAIL"; fi
  echo "===== $name (check_rc=$chk_rc vm_rc=$vm_rc comp_rc=$crc) [$status] ====="
  if [ "$status" != "OK" ] || [ "$chk_rc" != "0" ]; then
    echo "--- check(tail) ---"; echo "$chk" | tail -4
    echo "--- VM ---"; echo "$vm" | head -22
    echo "--- BUILD+PY3.13 ---"; echo "$comp" | head -22
  fi
done
