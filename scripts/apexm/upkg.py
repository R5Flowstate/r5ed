"""Minimal cooked UE4 (unversioned, legacy -7) package reader: summary, names, imports, exports."""
import struct, sys, os

class R:
    def __init__(s, b, o=0): s.b=b; s.o=o
    def u8(s): v=s.b[s.o]; s.o+=1; return v
    def i32(s): v=struct.unpack_from('<i',s.b,s.o)[0]; s.o+=4; return v
    def u32(s): v=struct.unpack_from('<I',s.b,s.o)[0]; s.o+=4; return v
    def i64(s): v=struct.unpack_from('<q',s.b,s.o)[0]; s.o+=8; return v
    def u16(s): v=struct.unpack_from('<H',s.b,s.o)[0]; s.o+=2; return v
    def f32(s): v=struct.unpack_from('<f',s.b,s.o)[0]; s.o+=4; return v
    def raw(s,n): v=s.b[s.o:s.o+n]; s.o+=n; return v
    def fstr(s):
        n=s.i32()
        if n==0: return ''
        if n<0: v=s.raw(-n*2).decode('utf-16le'); return v[:-1]
        return s.raw(n)[:-1].decode('latin-1')

class Package:
    def __init__(s, path):
        s.path=path; b=open(path,'rb').read(); s.b=b; r=R(b)
        assert r.u32()==0x9E2A83C1
        s.legacy=r.i32(); assert s.legacy==-7, s.legacy
        r.i32(); s.ue4=r.i32(); s.lic=r.i32(); ncv=r.i32(); r.raw(20*ncv)
        s.header_size=r.i32(); s.folder=r.fstr(); s.flags=r.u32()
        s.name_count=r.i32(); s.name_off=r.i32()
        r.i32(); r.i32()  # gatherable text
        s.export_count=r.i32(); s.export_off=r.i32(); s.import_count=r.i32(); s.import_off=r.i32(); s.depends_off=r.i32()
        r.i32(); r.i32(); r.i32(); r.i32()  # soft refs count/off, searchable names, thumbnails
        s.guid=r.raw(16); ng=r.i32(); r.raw(8*ng)
        for _ in range(2): r.u16(); r.u16(); r.u16(); r.u32(); r.fstr()
        r.u32(); assert r.i32()==0  # compression, chunks
        r.u32(); nap=r.i32()
        for _ in range(nap): r.fstr()
        s.asset_registry_off=r.i32(); s.bulk_start=r.i64(); r.i32(); nchunk=r.i32(); r.raw(4*nchunk)
        s.preload_count=r.i32(); s.preload_off=r.i32()
        r=R(b,s.name_off); s.names=[]
        for _ in range(s.name_count): s.names.append(r.fstr()); r.u32()  # hash pair
        r=R(b,s.import_off); s.imports=[]
        for _ in range(s.import_count):
            cp=s.fname(r); cn=s.fname(r); outer=r.i32(); on=s.fname(r)
            s.imports.append(dict(class_package=cp,class_name=cn,outer=outer,name=on))
        r=R(b,s.export_off); s.exports=[]
        for _ in range(s.export_count):
            e=dict(class_index=r.i32(),super_index=r.i32(),template_index=r.i32(),outer=r.i32(),name=s.fname(r),
                   object_flags=r.u32(),serial_size=r.i64(),serial_offset=r.i64(),forced=r.i32(),not_client=r.i32(),
                   not_server=r.i32(),pkg_guid=r.raw(16),pkg_flags=r.u32(),not_always_loaded=r.i32(),is_asset=r.i32(),
                   first_dep=r.i32(),deps=[r.i32() for _ in range(4)])
            e['class']=s.ref(e['class_index']); s.exports.append(e)
        base=os.path.splitext(path)[0]
        s.uexp=open(base+'.uexp','rb').read() if os.path.exists(base+'.uexp') else b''
        s.ubulk_path=base+'.ubulk'
    def fname(s, r):
        i=r.i32(); n=r.i32(); nm=s.names[i]
        return nm if n==0 else f'{nm}_{n-1}'
    def ref(s, idx):
        if idx<0: return s.imports[-idx-1]['name']
        if idx>0: return s.exports[idx-1]['name']
        return None
    def export_bytes(s, e):
        """Export payload: offsets count from the start of .uasset, the payload lives in .uexp right after the header."""
        off=e['serial_offset']-s.header_size
        return s.uexp[off:off+e['serial_size']]

if __name__=='__main__':
    p=Package(sys.argv[1])
    print('ue4',p.ue4,'lic',p.lic,'flags',hex(p.flags),'header',p.header_size,'bulk_start',p.bulk_start,'uexp',len(p.uexp))
    for i in p.imports: print(' import',i['class_package'],i['class_name'],i['name'],i['outer'])
    for e in p.exports: print(' export',e['class'],e['name'],'off',e['serial_offset'],'size',e['serial_size'],'outer',e['outer'])
