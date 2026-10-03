"""Portable W2 checker corpus comparison; no builds or source mutations."""
import concurrent.futures, json, pathlib, subprocess, sys, tomllib, os
root = pathlib.Path(__file__).resolve().parents[2]
binary = str(pathlib.Path(sys.argv[1]).resolve())
output = pathlib.Path(sys.argv[2])
projects = sorted(p.parent for r in ('examples','stress') for p in (root/r).rglob('typhon.toml'))
files = sorted(p for r in ('examples','stress') for p in (root/r).rglob('*.ty') if not any(p.is_relative_to(d) for d in projects))
units = projects + files
baseline = set(x for x in (root/'scripts/nobuild-baseline.txt').read_text().splitlines() if x and not x.startswith('#'))
def check(p):
    ident = str(p.relative_to(root)) + ('/' if p.is_dir() else '')
    if p.is_dir():
        cfg = tomllib.loads((p/'typhon.toml').read_text())
        src = cfg.get('project',{}).get('src',cfg.get('build',{}).get('src','src'))
        target = str(p/src) if (p/src).exists() else str(p)
    else: target = str(p)
    try:
        r = subprocess.run([binary,'check',target],cwd=p if p.is_dir() else root,env={**os.environ,'TYC_NO_INTROSPECT':'1'},capture_output=True,text=True,timeout=30)
        return ident, {'exit':r.returncode,'output':r.stdout+r.stderr,'baseline_rejected':ident in baseline}
    except subprocess.TimeoutExpired: return ident,{'exit':124,'output':'TIMEOUT','baseline_rejected':ident in baseline}
with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
    result = dict(pool.map(check,units))
output.write_text(json.dumps(result,indent=2))
print(json.dumps({'units':len(result),'accepted':sum(v['exit']==0 for v in result.values()),'rejected':sum(v['exit']!=0 for v in result.values()),'rejected_outside_baseline':[k for k,v in result.items() if v['exit']!=0 and not v['baseline_rejected']]},indent=2))
