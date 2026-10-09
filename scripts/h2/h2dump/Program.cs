using System.Numerics;
using System.Text.Json;
using Reclaimer.Blam.Common;
using Reclaimer.Blam.Halo2;
using Reclaimer.Geometry;
using Reclaimer.IO;
using Reclaimer.Blam.Utilities;

var mapPath = args[0];
var outDir = args[1];
Directory.CreateDirectory(Path.Combine(outDir, "parts"));
Directory.CreateDirectory(Path.Combine(outDir, "textures"));

var cache = new CacheFile(mapPath);
var scnrItem = cache.TagIndex.First(t => t.ClassCode == "scnr");
var scnr = scnrItem.ReadMetadata<ScenarioTag>();
var scene = scnr.GetContent();

var parts = new List<object>();
var materials = new Dictionary<string, object>();
var textures = new Dictionary<string, string>();
int partId = 0;

string Safe(string s) => string.Concat(s.Select(c => char.IsLetterOrDigit(c) || c == '_' || c == '-' ? c : '_'));

string TextureFile(Texture tex)
{
    if (tex?.ContentProvider == null) return null;
    var key = tex.Name ?? $"tex{tex.Id}";
    if (textures.TryGetValue(key, out var f)) return f;
    f = "textures/" + Safe(key) + ".dds";
    try { tex.GetDds().WriteToDisk(Path.Combine(outDir, f)); }
    catch (Exception e) { Console.WriteLine($"tex fail {key}: {e.Message}"); f = null; }
    textures[key] = f;
    return f;
}

string MaterialKey(Material m)
{
    if (m == null) return "none";
    var key = m.Name ?? $"mat{m.Id}";
    if (!materials.ContainsKey(key))
    {
        materials[key] = new
        {
            name = key,
            alpha = m.AlphaMode,
            textures = m.TextureMappings.Select(t => new
            {
                usage = t.Usage,
                name = t.Texture?.Name,
                file = TextureFile(t.Texture),
                tiling = new[] { t.Tiling.X, t.Tiling.Y },
                channel = t.BlendChannel.ToString()
            }).ToList(),
            tints = m.Tints.Select(t => new { usage = t.Usage, rgba = new[] { t.Color.R, t.Color.G, t.Color.B, t.Color.A } }).ToList()
        };
    }
    return key;
}

void DumpModel(Model model, Matrix4x4 world, string group, string instance)
{
    foreach (var region in model.Regions)
    {
        foreach (var perm in region.Permutations)
        {
            var xf = perm.GetFinalTransform() * world;
            var nxf = Matrix4x4.Invert(xf, out var inv) ? Matrix4x4.Transpose(inv) : xf;
            foreach (var mi in perm.MeshIndices)
            {
                if (mi >= model.Meshes.Count) continue;
                var mesh = model.Meshes[mi];
                if (mesh?.VertexBuffer == null || mesh.IndexBuffer == null || mesh.VertexCount == 0) continue;
                var posX = mesh.PositionBounds.IsEmpty ? Matrix4x4.Identity : mesh.PositionBounds.CreateExpansionMatrix();
                var uvX = mesh.TextureBounds.IsEmpty ? Matrix4x4.Identity : mesh.TextureBounds.CreateExpansionMatrix();
                var pos = mesh.GetPositions().Select(p => Vector3.Transform(Vector3.Transform(p, posX), xf)).ToArray();
                var nrm = mesh.GetNormals()?.Select(n => Vector3.Normalize(Vector3.TransformNormal(n, nxf))).ToArray() ?? new Vector3[pos.Length];
                var uvs = mesh.GetTexCoords()?.Select(t => Vector2.Transform(t, uvX)).ToArray() ?? new Vector2[pos.Length];
                foreach (var seg in mesh.Segments)
                {
                    var idx = mesh.GetTriangleIndicies(seg).ToArray();
                    if (idx.Length < 3) continue;
                    var used = idx.Distinct().OrderBy(i => i).ToArray();
                    var remap = new Dictionary<int, int>();
                    for (int i = 0; i < used.Length; i++) remap[used[i]] = i;
                    var file = $"parts/{partId:D5}.bin";
                    using (var bw = new BinaryWriter(File.Create(Path.Combine(outDir, file))))
                    {
                        foreach (var u in used) { var p = pos[u]; bw.Write(p.X); bw.Write(p.Y); bw.Write(p.Z); }
                        foreach (var u in used) { var n = nrm[u]; bw.Write(n.X); bw.Write(n.Y); bw.Write(n.Z); }
                        foreach (var u in used) { var t = uvs[u]; bw.Write(t.X); bw.Write(t.Y); }
                        foreach (var i in idx) bw.Write(remap[i]);
                    }
                    parts.Add(new
                    {
                        id = partId, file, group, instance, region = region.Name, permutation = perm.Name,
                        material = MaterialKey(seg.Material), verts = used.Length, indices = idx.Length
                    });
                    partId++;
                }
            }
        }
    }
}

void Walk(SceneGroup g, string path)
{
    var gp = path == null ? g.Name : path + "/" + g.Name;
    int n = 0;
    foreach (var obj in g.ChildObjects)
    {
        var world = obj.Transform;
        SceneObject o = obj.Object;
        while (o is ObjectPlacement inner) { world = inner.Transform * world; o = inner.Object; }
        if (o is Model m) DumpModel(m, world, gp, $"{m.Name}#{n}");
        n++;
    }
    foreach (var c in g.ChildGroups) Walk(c, gp);
}
Walk(scene.RootNode, null);

// Every bitmap and constant the map's shaders bind. The material reader only
// resolves usages for Xbox shader passes, so the raw template data is kept too.
var bitmapFiles = new Dictionary<string, string>();
string ExportBitmap(IIndexItem t)
{
    if (bitmapFiles.TryGetValue(t.TagName, out var f)) return f;
    f = "textures/" + Safe(t.TagName) + ".dds";
    try { t.ReadMetadata<BitmapTag>().GetContent().ToDds(0).WriteToDisk(Path.Combine(outDir, f)); }
    catch (Exception e) { Console.WriteLine($"bitmap fail {t.TagName}: {e.Message}"); f = null; }
    bitmapFiles[t.TagName] = f;
    return f;
}
var shaders = new Dictionary<string, object>();
var shaderTags = scnr.StructureBsps
    .Where(b => b.BspReference.IsValid)
    .SelectMany(b => b.BspReference.Tag.ReadMetadata<ScenarioStructureBspTag>().Shaders)
    .Select(s => s.ShaderReference.Tag)
    .Where(t => t != null)
    .GroupBy(t => t.TagName).Select(g => g.First());
// A shader shared with other maps lives in shared.map; its tag ids index that cache.
CacheFile sharedCache = null;
foreach (var item0 in shaderTags)
{
    try
    {
        var (c, item) = ((IndexItem)item0).IsShared
            ? (sharedCache ??= new CacheFile(Path.Combine(Path.GetDirectoryName(mapPath), CacheFile.SharedMap)), sharedCache.TagIndex[item0.Id])
            : (cache, (IndexItem)item0);
        var str = item.GetAddressTranslator();
        using var r = c.CreateReader(str);
        long b = item.MetaPointer.Address;
        (int n, long a) Blk(long at)
        {
            r.Seek(at, SeekOrigin.Begin);
            var n = r.ReadInt32();
            return (n, new Pointer(r.ReadInt32(), str).Address);
        }
        var names = (Dictionary<int, string>)typeof(TagIndex).GetProperty("TagNames", System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Instance).GetValue(c.TagIndex);
        IIndexItem TagAt(int id)
        {
            if (id == -1) return null;
            try { return c.TagIndex[id & 0xFFFF]; } catch (KeyNotFoundException) { return null; }
        }
        string NameAt(int id) => id == -1 ? null : names.TryGetValue(id & 0xFFFF, out var n) ? n : null;
        r.Seek(b + 4, SeekOrigin.Begin);
        var template = NameAt(r.ReadInt32());

        var parameters = new List<object>();
        var (pn, pa) = Blk(b + 0x18);
        for (int i = 0; i < pn; i++)
        {
            var e = pa + i * 0x28;
            r.Seek(e, SeekOrigin.Begin);
            var sid = r.ReadInt32();
            var type = r.ReadInt16();
            r.Seek(e + 0xC, SeekOrigin.Begin);
            var bid = r.ReadInt32();
            var bm = TagAt(bid);
            var value = r.ReadSingle();
            var color = new[] { r.ReadSingle(), r.ReadSingle(), r.ReadSingle() };
            parameters.Add(new { name = c.StringIndex[sid], sid, type, bitmap = NameAt(bid), local = bm != null, file = bm == null ? null : ExportBitmap(bm), value, color });
        }

        var ppBitmaps = new List<object>();
        var ppConsts = new List<float[]>();
        string ppTemplate = null;
        var (qn, qa) = Blk(b + 0x20);
        if (qn > 0)
        {
            r.Seek(qa, SeekOrigin.Begin);
            ppTemplate = NameAt(r.ReadInt32());
            var (bn, ba) = Blk(qa + 0x4);
            for (int i = 0; i < bn; i++)
            {
                r.Seek(ba + i * 0xC, SeekOrigin.Begin);
                var bid = r.ReadInt32();
                var bm = TagAt(bid);
                var index = r.ReadInt32();
                var logDim = r.ReadSingle();
                ppBitmaps.Add(new { slot = i, bitmap = NameAt(bid), local = bm != null, file = bm == null ? null : ExportBitmap(bm), index, logDim });
            }
            var (vn, va) = Blk(qa + 0x14);
            for (int i = 0; i < vn; i++)
            {
                r.Seek(va + i * 0x10, SeekOrigin.Begin);
                ppConsts.Add(new[] { r.ReadSingle(), r.ReadSingle(), r.ReadSingle(), r.ReadSingle() });
            }
        }
        shaders[item0.FileName] = new { tag = item0.TagName, shared = c != cache, template, ppTemplate, parameters, bitmaps = ppBitmaps, constants = ppConsts };
    }
    catch (Exception e) { Console.WriteLine($"shad fail {item0.TagName}: {e.Message}"); }
}

// Raw scenario blocks for gameplay markers (H2 layout).
var tr = scnrItem.GetAddressTranslator();
using var reader = cache.CreateReader(tr);
var baseAddr = scnrItem.MetaPointer.Address;
(int count, long addr) Block(int off)
{
    reader.Seek(baseAddr + off, SeekOrigin.Begin);
    var c = reader.ReadInt32();
    var p = new Pointer(reader.ReadInt32(), tr);
    return (c, p.Address);
}
string TagRefName(long at)
{
    reader.Seek(at + 4, SeekOrigin.Begin);
    var id = reader.ReadInt32();
    if (id == -1) return null;
    try { var t = cache.TagIndex[id & 0xFFFF]; return $"{t.ClassCode}:{t.TagName}"; } catch { return $"?{id:X8}"; }
}
float[] V3(long at) { reader.Seek(at, SeekOrigin.Begin); return new[] { reader.ReadSingle(), reader.ReadSingle(), reader.ReadSingle() }; }
List<string> Palette(int off)
{
    var (c, a) = Block(off);
    var l = new List<string>();
    for (int i = 0; i < c; i++) l.Add(TagRefName(a + i * 0x30));
    return l;
}
List<object> Placements(int off, int palOff, int size)
{
    var pal = Palette(palOff);
    var (c, a) = Block(off);
    var l = new List<object>();
    for (int i = 0; i < c; i++)
    {
        var e = a + i * size;
        reader.Seek(e, SeekOrigin.Begin);
        var pi = reader.ReadInt16();
        l.Add(new { palette = pi >= 0 && pi < pal.Count ? pal[pi] : null, position = V3(e + 8), rotation = V3(e + 0x14), scale = V3(e + 0x20)[0] });
    }
    return l;
}

var starts = new List<object>();
{
    var (c, a) = Block(0x100);
    for (int i = 0; i < c; i++)
    {
        var e = a + i * 0x34;
        var p = V3(e);
        reader.Seek(e + 0xC, SeekOrigin.Begin);
        var facing = reader.ReadSingle(); var team = reader.ReadInt16(); reader.ReadInt16();
        var gt = new[] { reader.ReadInt16(), reader.ReadInt16(), reader.ReadInt16(), reader.ReadInt16() };
        var st = new[] { reader.ReadInt16(), reader.ReadInt16(), reader.ReadInt16(), reader.ReadInt16() };
        starts.Add(new { position = p, facing, team, gametypes = gt, spawn_types = st });
    }
}
var flags = new List<object>();
{
    var (c, a) = Block(0x118);
    for (int i = 0; i < c; i++)
    {
        var e = a + i * 0x20;
        var p = V3(e);
        reader.Seek(e + 0xC, SeekOrigin.Begin);
        flags.Add(new { position = p, facing = reader.ReadSingle(), type = reader.ReadInt16(), team = reader.ReadInt16(), identifier = reader.ReadInt16(), flags = reader.ReadInt16() });
    }
}
var equipment = new List<object>();
{
    var (c, a) = Block(0x120);
    for (int i = 0; i < c; i++)
    {
        var e = a + i * 0x90;
        reader.Seek(e + 0xE, SeekOrigin.Begin);
        var spawnTime = reader.ReadInt16();
        equipment.Add(new { position = V3(e + 0x40), orientation = V3(e + 0x4C), collection = TagRefName(e + 0x58), spawn_time = spawnTime });
    }
}

var gameplay = new
{
    map = cache.FileName,
    scenario = scnrItem.TagName,
    player_starts = starts,
    netgame_flags = flags,
    netgame_equipment = equipment,
    vehicles = Placements(0x70, 0x78, 0x54),
    weapons = Placements(0x90, 0x98, 0x54),
    scenery = Placements(0x50, 0x58, 0x5C),
};

var jo = new JsonSerializerOptions { WriteIndented = true, IncludeFields = true };
File.WriteAllText(Path.Combine(outDir, "parts.json"), JsonSerializer.Serialize(parts, jo));
File.WriteAllText(Path.Combine(outDir, "materials.json"), JsonSerializer.Serialize(materials, jo));
File.WriteAllText(Path.Combine(outDir, "shaders.json"), JsonSerializer.Serialize(shaders, jo));
File.WriteAllText(Path.Combine(outDir, "gameplay.json"), JsonSerializer.Serialize(gameplay, jo));
Console.WriteLine($"parts={parts.Count} materials={materials.Count} textures={textures.Count} starts={starts.Count} flags={flags.Count} equip={equipment.Count}");
