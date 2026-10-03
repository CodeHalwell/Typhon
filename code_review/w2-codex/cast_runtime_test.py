"""Tests the actual generated runtime template; needs CPython 3.13+."""
import ast
from pathlib import Path
import typing
import unittest
source = (Path(__file__).resolve().parents[2]/'tyc/crates/tyc/src/commands/build.rs').read_text()
start = source.index('const TYPHON_RUNTIME_CAST_PY: &str = "') + len('const TYPHON_RUNTIME_CAST_PY: &str = "')
end = source.index('\n";', start)
namespace = {}
exec(ast.literal_eval('"""'+source[start:end]+'"""'), namespace)
checked_cast = namespace['checked_cast']
type Payload = dict[str, int]
type Vec[T] = list[T]
type Nest[T] = dict[str, Vec[T]]
type Tree = int | list[Tree]
Email = typing.NewType('Email',str)
class RuntimeCastTests(unittest.TestCase):
    def test_aliases_and_literals_reject_bad_values(self):
        for target,value in [(Payload,{'a':'x'}),(Vec[str],[1]),(Nest[str],{'a':[1]}),(typing.Literal['yes','no'],'maybe'),(typing.Literal[1],True),(Email,42)]:
            with self.subTest(target=target):
                with self.assertRaises(TypeError): checked_cast(value,target)
    def test_supported_targets_preserve_identity(self):
        for target,value in [(Payload,{'a':1}),(Vec[str],['a']),(Nest[str],{'a':['b']}),(typing.Literal['yes','no'],'yes'),(Email,'a@b'),(float,1),(tuple[int,...],(1,2)),(int|None,None)]:
            with self.subTest(target=target): self.assertIs(checked_cast(value,target),value)
    def test_recursive_alias_and_cyclic_value(self):
        value = [1,[2]]
        self.assertIs(checked_cast(value,Tree),value)
        with self.assertRaises(TypeError): checked_cast(['bad'],Tree)
        cycle = []
        cycle.append(cycle)
        with self.assertRaises(TypeError): checked_cast(cycle,Tree)
    def test_unbound_descriptors_are_refused(self):
        with self.assertRaises(TypeError): checked_cast(1,typing.TypeVar('T'))
if __name__ == '__main__': unittest.main()
