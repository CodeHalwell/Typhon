"""Recursive freeze controls against the actual runtime template."""
import ast
import dataclasses
from pathlib import Path
from types import MappingProxyType
import unittest
source = (Path(__file__).resolve().parents[2]/'tyc/crates/tyc/src/commands/build.rs').read_text()
start = source.index('const TYPHON_RUNTIME_FREEZE_PY: &str = "') + len('const TYPHON_RUNTIME_FREEZE_PY: &str = "')
end = source.index('\n";',start)
namespace = {}
exec(ast.literal_eval('"""'+source[start:end]+'"""'),namespace)
freeze = namespace['deep_freeze']
@dataclasses.dataclass(frozen=True)
class Config:
    values: list[int]
class FreezeRuntimeTests(unittest.TestCase):
    def test_mapping_proxy_contents_are_recursive(self):
        source = MappingProxyType({'values':[1]})
        result = freeze(source)
        self.assertEqual(result['values'],(1,))
        self.assertEqual(source['values'],[1])
    def test_frozen_instance_identity_and_fields_are_preserved(self):
        value = Config([1])
        self.assertIs(freeze(value),value)
        self.assertIs(freeze([value])[0],value)
        self.assertEqual(value.values,[1])
    def test_nested_mutable_shapes_and_cycles(self):
        value = freeze({'items':[[1]],'set':{2}})
        self.assertEqual(value['items'],((1,),))
        self.assertEqual(value['set'],frozenset({2}))
        cycle=[]
        cycle.append(cycle)
        with self.assertRaises(TypeError): freeze(cycle)
if __name__ == '__main__': unittest.main()
