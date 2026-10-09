"""Tagged property reader for cooked UE4 (4.20-4.27) exports. Values decoded for the common types, others kept raw."""
import struct
from upkg import R

def read_tag(p, r):
    name=p.fname(r)
    if name=='None': return None
    t=dict(name=name,type=p.fname(r),size=r.i32(),index=r.i32())
    ty=t['type']
    if ty=='StructProperty': t['struct']=p.fname(r); t['struct_guid']=r.raw(16)
    elif ty=='BoolProperty': t['bool']=r.u8()
    elif ty in ('ByteProperty','EnumProperty'): t['enum']=p.fname(r)
    elif ty in ('ArrayProperty','SetProperty'): t['inner']=p.fname(r)
    elif ty=='MapProperty': t['inner']=p.fname(r); t['value']=p.fname(r)
    if r.u8(): t['guid']=r.raw(16)
    return t

def decode(p, t, raw):
    ty=t['type']; r=R(raw)
    try:
        if ty=='BoolProperty': return bool(t['bool'])
        if ty=='IntProperty': return r.i32()
        if ty=='UInt32Property': return r.u32()
        if ty=='FloatProperty': return r.f32()
        if ty=='ObjectProperty': return ('obj', p.ref(r.i32()))
        if ty=='NameProperty': return p.fname(r)
        if ty=='StrProperty': return r.fstr()
        if ty in ('ByteProperty','EnumProperty'):
            return p.fname(r) if len(raw)==8 else raw[0]
        if ty=='StructProperty' and t['struct'] in ('Vector','Rotator'): return struct.unpack('<3f',raw)
        if ty=='StructProperty' and t['struct']=='Vector2D': return struct.unpack('<2f',raw)
        if ty=='StructProperty' and t['struct'] in ('Guid',): return raw.hex()
        if ty=='ArrayProperty' and t['inner']=='ObjectProperty':
            n=r.i32(); return [('obj',p.ref(r.i32())) for _ in range(n)]
    except Exception as e:
        return ('?',raw.hex())
    return ('raw',len(raw))

def read_props(p, r, decode_values=True):
    out=[]
    while True:
        t=read_tag(p,r)
        if t is None: break
        raw=r.raw(t['size'])
        t['raw']=raw
        if decode_values: t['value']=decode(p,t,raw)
        out.append(t)
    return out
