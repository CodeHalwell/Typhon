"""Run the two newly rejected interface probes with their CPython lowering."""
from pathlib import Path
import re
ROOT = Path(__file__).resolve().parents[2]
for unit, expected in [
    ('stress/round-2026-09-01/types/h34_frozen_iface.ty', 'FrozenInstanceError'),
    ('stress/round-2026-09-01/types/n5_iface_field_covariant.ty', 'AttributeError'),
]:
    lines = ['from dataclasses import dataclass', 'from typing import Protocol']
    for line in (ROOT/unit).read_text().splitlines():
        if line.startswith('interface '):
            line = 'class '+line[len('interface '):-1]+'(Protocol):'
        elif line.startswith('class '):
            frozen = ' frozen:' in line
            lines.append('@dataclass(slots=True'+(', frozen=True' if frozen else '')+')')
            line = line.replace(' frozen:', ':')
        line = re.sub(r'^(\s*)let ', r'\1', line)
        lines.append(line)
    try:
        exec(compile('\n'.join(lines), unit, 'exec'), {'__name__': '__main__'})
    except Exception as error:
        assert type(error).__name__ == expected, (unit, error)
        print(unit, type(error).__name__, str(error))
    else:
        raise AssertionError(unit+' should fail')
