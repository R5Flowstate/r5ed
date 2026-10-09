import glob, os, pickle, numpy as np
from upkg import Package
from umesh import static_mesh
ROOT = os.environ.get('APEXM_BACKUP', '.')
_index=None
def find_mesh(name):
    global _index
    if _index is None:
        _index={}
        for sub in ('cyberpark_uefiles','Environment','new','AtlasMergeMetas'):
            for f in glob.glob(f'{ROOT}/{sub}/**/*.uasset',recursive=True):
                _index.setdefault(os.path.basename(f)[:-7],f)
    return _index.get(name)
_cache={}
def load(name):
    if name in _cache: return _cache[name]
    f=find_mesh(name); m=None
    if f:
        p=Package(f); e=[x for x in p.exports if x['class']=='StaticMesh']
        if e: m=(p,static_mesh(p,e[0]),f)
    _cache[name]=m; return m
def world_tris(placements, skip_levels=('TDM_MP_CyberPark_SkyBox.umap',)):
    for pl in placements:
        if pl['hidden'] or pl['level'] in skip_levels: continue
        got=load(pl['mesh'])
        if not got: continue
        p,m,f=got; l=m['lods'][0]
        if 'positions' not in l: continue
        V=np.c_[l['positions'],np.ones(len(l['positions']))]
        for M in pl['matrices']:
            W=(V@M)[:,:3]
            yield pl,m,l,W
