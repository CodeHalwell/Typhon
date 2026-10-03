from pathlib import Path
for path,fn,args in [
 ('stress/round-2026-06-21/repros/118-cast-partial.ty','power',(2,-1)),
 ('stress/round-2026-06-21/repros/78-deep-recursion-bignum.ty','power_tower',(-2,2)),
]:
 ns={};exec(Path(path).read_text().split('def main()')[0],ns)
 try: ns[fn](*args).bit_length()
 except AttributeError as e:print(path,fn,args,type(e).__name__,str(e))
 else:raise AssertionError('expected float in int contract')
try:
 n=2**-1;n.bit_length()
except AttributeError as e:print('stress/round-2026-09-01/types/h101_pow.ty',type(e).__name__,str(e))
