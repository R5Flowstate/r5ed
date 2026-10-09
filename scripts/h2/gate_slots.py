"""Gate: every guid-ref slot of an rpak is listed once and owned by exactly one asset.
The engine patches each listed slot from a guid to an asset pointer as it resolves it,
so a slot listed twice is looked up a second time as a pointer and the load fails with
"Pak file consistency error." usage: gate_slots.py <pak>"""
import struct, sys
from collections import Counter
sys.path.insert(0, __import__("os").path.join(__import__("os").path.dirname(__import__("os").path.dirname(__import__("os").path.abspath(__file__))), "lib"))
import rpak

pk = rpak.open_pak(sys.argv[1])
slots = Counter((p.index, p.offset) for p in pk.guid_refs)
owned = Counter()
bad = 0
for i, a in enumerate(pk.assets):
    b = pk.assets_off + 0x50 * i
    first, = struct.unpack_from("<I", pk.buf, b + 0x38)
    count, = struct.unpack_from("<H", pk.buf, b + 0x40)
    dup = 0
    for k in range(first, first + count):
        owned[k] += 1
        p = pk.guid_refs[k]
        dup += slots[(p.index, p.offset)] > 1
    if dup:
        bad += 1
        print("  %s 0x%016X: %d of %d guid slots listed twice" % (a.type, a.guid, dup, count))
unowned = sum(1 for k in range(len(pk.guid_refs)) if owned[k] != 1)
print("%s: guid-ref slots %d  DUPLICATE %d  bad-owner %d"
      % (sys.argv[1].replace("\\", "/").split("/")[-1], len(pk.guid_refs),
         sum(1 for v in slots.values() if v > 1), unowned))
sys.exit(1 if bad or unowned else 0)
