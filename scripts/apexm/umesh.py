"""Apex Legends Mobile (UE 4.23 + Tencent changes) cooked UStaticMesh reader."""
import struct, numpy as np
from upkg import R
from uprops import read_props

class Fail(Exception): pass

def bounds(r):
    return dict(origin=struct.unpack('<3f',r.raw(12)), extent=struct.unpack('<3f',r.raw(12)), radius=r.f32())

def section(r):
    s=dict(material=r.i32(), first_index=r.u32(), num_tris=r.u32(), min_vert=r.u32(), max_vert=r.u32(),
           collision=r.u32(), cast_shadow=r.u32())
    s['bounds']=bounds(r)
    n=r.u32(); s['u16s']=[r.u16() for _ in range(n)]
    n=r.u32(); s['clusters']=[r.raw(16) for _ in range(n)]
    return s

def bulk_array(r):
    es=r.i32(); n=r.i32()
    return es, n, r.raw(es*n)

def index_buffer(r):
    is32=r.u32(); es,n,raw=bulk_array(r)
    assert es==1, ('index elemsize',es)
    idx=np.frombuffer(raw,dtype='<u4' if is32 else '<u2')
    return idx

def buffers(r, nsec, mesh_bounds):
    lod={}
    sf=(r.u8(),r.u8()); lod['buf_strip']=sf
    # position
    stride=r.u32(); nv=r.u32(); es,n,raw=bulk_array(r)
    if n==nv and es==12:
        pos=np.frombuffer(raw,dtype='<f4').reshape(-1,3); lod['pos_format']='float32'
    elif n==0:
        q=np.frombuffer(r.raw(6*nv),dtype='<i2').reshape(-1,3).astype(np.float64)
        o=np.array(mesh_bounds['origin']); e=np.array(mesh_bounds['extent'])
        pos=o+q/32767.0*e; lod['pos_format']='snorm16 x bounds'
    else: raise Fail(('position',stride,nv,es,n))
    lod['positions']=pos; lod['num_verts']=nv
    # static mesh vertex buffer
    vs=(r.u8(),r.u8()); ntex=r.u32(); nv2=r.u32(); fulluv=r.u32(); hitan=r.u32()
    if nv2!=nv: raise Fail(('smvb nv',nv,nv2))
    es,n,traw=bulk_array(r); lod['tangent_elem']=es; lod['tangents_raw']=np.frombuffer(traw,np.uint8).reshape(nv,-1) if nv else None
    es2,n2,uraw=bulk_array(r)
    uvt='<f4' if fulluv else '<f2'
    uv=np.frombuffer(uraw,dtype=uvt).reshape(nv,ntex,2) if nv else None
    lod['uv']=uv; lod['num_uv']=ntex; lod['full_uv']=fulluv; lod['hi_tangent']=hitan; lod['uv_elem']=es2
    # colour streams: stock ColorVertexBuffer + two more of the same shape
    lod['colors']=[]
    for _ in range(3):
        cs=(r.u8(),r.u8()); cstride=r.u32(); cn=r.u32()
        col=None
        if cn:
            es,n,craw=bulk_array(r); col=np.frombuffer(craw,np.uint8).reshape(cn,-1)
        lod['colors'].append((cstride,cn,col))
    lod['indices']=index_buffer(r)
    strip_class=sf[1]
    if not strip_class & 4: lod['reversed']=index_buffer(r)
    lod['depth_only']=index_buffer(r)
    if not strip_class & 4: lod['reversed_depth']=index_buffer(r)
    if not sf[0] & 1: lod['wireframe']=index_buffer(r)
    if not strip_class & 1: lod['adjacency']=index_buffer(r)
    # area weighted samplers: one per section + one global (TArray<float>, TArray<int32>, float)
    for _ in range(nsec+1):
        a=r.i32(); r.raw(4*a); b=r.i32(); r.raw(4*b); r.f32()
    return lod

def lod_resources(r, mesh_bounds, bulk_reader=None):
    size=r.u32(); hi=r.u32()
    if hi: raise Fail(('lod size hi',hi))
    start=r.o
    strip=(r.u8(),r.u8()); ns=r.u32(); secs=[section(r) for _ in range(ns)]
    maxdev=r.f32(); cooked_out=r.u32(); inlined=r.u32()
    lod=dict(strip=strip,sections=secs,max_deviation=maxdev,cooked_out=cooked_out,inlined=inlined)
    if not cooked_out:
        if inlined: lod.update(buffers(r, ns, mesh_bounds))
        else:
            # FByteBulkData: flags, element count, size on disk, offset (relative to the package's BulkDataStartOffset)
            flags=r.u32(); count=r.i32(); disk=r.i32(); off=r.i64()
            lod['bulk']=dict(flags=flags,count=count,size_on_disk=disk,offset=off)
            # SerializeAvailabilityInfo: DepthOnlyNumTriangles, packed flags, then the metadata of every buffer;
            # Apex Mobile carries three colour streams, so three colour metadata pairs
            lod['depth_only_tris']=r.u32(); lod['packed']=r.u32()
            r.raw(16+8+3*8+6*8)
            if bulk_reader is not None and count>0:
                data=bulk_reader(off, count)
                br=R(data); lod.update(buffers(br, ns, mesh_bounds))
                if br.o!=len(data): raise Fail(('bulk lod end',br.o,len(data)))
    lod['buffer_sizes']=(r.u32(),r.u32(),r.u32())
    if r.o!=start+size: raise Fail(('lod end',r.o-start,size))
    return lod

def static_mesh(p, e):
    b=p.export_bytes(e); r=R(b)
    m=dict(name=e['name']); m['props']={t['name']:t for t in read_props(p,r)}
    if r.i32(): r.raw(16)
    m['strip']=(r.u8(),r.u8()); m['cooked']=r.u32(); m['body_setup']=r.i32(); m['nav']=r.i32()
    m['lighting_guid']=r.raw(16); ns=r.i32(); m['sockets']=[r.i32() for _ in range(ns)]
    # The render bounds (the quantization box) come after the LODs, so find them first by skipping the LODs by size.
    nl=r.i32(); lod_start=r.o
    for _ in range(nl):
        sz=r.u32(); r.u32(); r.o+=sz
    m['num_inlined_lods']=r.u8(); m['tail_strip']=(r.u8(),r.u8())
    mb=bounds(r); m['bounds']=mb
    m['lods_share_static_lighting']=r.u32()
    m['screen_size']=[]
    for _ in range(8): r.u32(); m['screen_size'].append(r.f32())
    m['tail_offset']=r.o; m['tail']=b[r.o:]
    def bulk_reader(off, count):
        with open(p.ubulk_path,'rb') as fh:
            fh.seek(off+p.bulk_start); d=fh.read(count)
        if len(d)!=count: raise Fail(('ubulk short',off,count,len(d)))
        return d
    r.o=lod_start
    m['lods']=[lod_resources(r, mb, bulk_reader) for _ in range(nl)]
    return m
