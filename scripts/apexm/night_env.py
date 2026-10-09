"""Gives an already-ported project the night fog and colour grade that port_level.py --lighting night adds.

    python3 scripts/apexm/night_env.py <project dir>

For a project ported before port_level wrote them: without them the compiler adds a clear day's fog and
grade. A project that already has its own env_fog_controller or color_correction keeps it.
"""
import json, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from port_level import NIGHT_ENV, add_environment

P = sys.argv[1]
have = {o['kind'].get('classname', '').lower() for o in json.load(open(os.path.join(P, 'map.json')))['objects']
        if o['kind']['type'] == 'entity'}
todo = {c: kv for c, kv in NIGHT_ENV.items() if c not in have}
add_environment(P, todo)
print('added', ', '.join(todo) or 'nothing (already there)')
