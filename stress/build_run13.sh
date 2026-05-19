#!/bin/bash
TYC=/home/user/Typhon/tyc/target/release/tyc
mkdir -p builds
for tc in $@; do
    name=$(basename "$tc" .ty)
    workdir="builds/$name"
    rm -rf "$workdir"
    mkdir -p "$workdir/src"
    cp "$tc" "$workdir/src/main.ty"
    cat > "$workdir/typhon.toml" <<TOML
[project]
name = "$name"
version = "0.1.0"
src = "src"
out = "build"
[python]
target = "3.13"
[emit]
class-default = "dataclass"
format = false
[strictness]
no-implicit-any = true
unused-import = "warn"
exhaustive-match = "error"
methods-in-class-body = "warn"
[env]
required = []
TOML
    pushd "$workdir" > /dev/null
    "$TYC" build > build.out 2>&1
    bcode=$?
    if [ $bcode -eq 0 ]; then
        python3.13 build/main.py > run.out 2>&1
        rcode=$?
        echo "$name: build=$bcode run=$rcode"
    else
        echo "$name: build=$bcode FAILED"
    fi
    popd > /dev/null
done
