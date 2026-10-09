import subprocess,sys,time,ctypes,os
from ctypes import wintypes
cap=float(sys.argv[1])*2**30; log=open(sys.argv[2],'w'); cmd=sys.argv[3:]
class PMC(ctypes.Structure):
    _fields_=[('cb',wintypes.DWORD),('PageFaultCount',wintypes.DWORD),('PeakWorkingSetSize',ctypes.c_size_t),('WorkingSetSize',ctypes.c_size_t),('QuotaPeakPagedPoolUsage',ctypes.c_size_t),('QuotaPagedPoolUsage',ctypes.c_size_t),('QuotaPeakNonPagedPoolUsage',ctypes.c_size_t),('QuotaNonPagedPoolUsage',ctypes.c_size_t),('PagefileUsage',ctypes.c_size_t),('PeakPagefileUsage',ctypes.c_size_t)]
# Priority class: BAKE_PRIORITY=idle|below|normal. Idle (the old default) only gets CPU nothing else
# wants, so any background app starves the bake; below-normal still yields to the desktop.
PRIORITY={'idle':0x40,'below':0x4000,'normal':0x20}[os.environ.get('BAKE_PRIORITY','below')]
p=subprocess.Popen(cmd,stdout=log,stderr=subprocess.STDOUT,creationflags=PRIORITY)
h=ctypes.windll.kernel32.OpenProcess(0x1000|0x0400,False,p.pid); peak=0; last=None
while p.poll() is None:
    m=PMC(); m.cb=ctypes.sizeof(m); ctypes.windll.psapi.GetProcessMemoryInfo(h,ctypes.byref(m),m.cb)
    peak=max(peak,m.PagefileUsage)
    if m.PagefileUsage>cap: p.kill(); print('KILLED at %.1f GB'%(m.PagefileUsage/2**30)); break
    time.sleep(0.5)
    # Echo the newest PROGRESS line (the bake prints one every 10 s per stage) once it changes.
    try:
        with open(sys.argv[2],'rb') as f:
            f.seek(0,2); f.seek(max(0,f.tell()-4096))
            lines=[l for l in f.read().decode('utf-8','replace').splitlines() if l.startswith('PROGRESS')]
        if lines and lines[-1]!=last:
            last=lines[-1]; print(last,flush=True)
    except OSError: pass
print('exit',p.wait(),'peak commit %.2f GB'%(peak/2**30))
