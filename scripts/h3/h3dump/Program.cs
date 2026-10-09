using System.Numerics;
using System.Text.Json;
using Reclaimer.Blam.Common;
using Reclaimer.Blam.Halo3;
using Reclaimer.Geometry;
using Reclaimer.IO;

var mapPath = args[0];
var outDir = args[1];
Directory.CreateDirectory(Path.Combine(outDir, "parts"));
Directory.CreateDirectory(Path.Combine(outDir, "textures"));

// MCC Halo 3 maps need the cache factory (version-aware); plain CacheFile is H2-only.
var cache = CacheFactory.ReadCacheFile(mapPath);
var scnrItem = cache.TagIndex.First(t => t.ClassCode == "scnr");
var scnr = scnrItem.ReadMetadata<ScenarioTag>();
var scene = scnr.GetContent();

// World geometry only. Marker, Forge-palette and gameplay groups are recorded
// in gameplay.json as placements but never shipped as render/collision meshes.
static bool ShipGroup(string path)
{
    var seg = path.Split('/');
    if (seg.Any(s => s == "skies"))
        return false;
    var leaf = seg.Last();
    foreach (var p in new[] { "box_", "wall_", "nutblocker", "mp_", "null_", "resupply_", "warthog_drop_" })
    {
        if (leaf.StartsWith(p))
            return false;
    }
    return true;
}

var parts = new List<object>();
var materials = new Dictionary<string, object>();
var textures = new Dictionary<string, string>();
var markers = new List<object>();
var starts = new List<object>();
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
                    // S21 samples albedo at raw UV0, so bake the Halo shader's
                    // diffuse tiling into the stored UVs. Blend materials are
                    // composed onto 0-1 in h3mat and need no scaling.
                    var uvS = new Vector2(1, 1);
                    if (seg.Material != null && !seg.Material.TextureMappings.Any(t => t.Usage == "blend"))
                    {
                        var d = seg.Material.TextureMappings.FirstOrDefault(t => t.Usage == "diffuse");
                        if (d != null) uvS = new Vector2(d.Tiling.X, d.Tiling.Y);
                    }
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
                        foreach (var u in used) { var t = uvs[u]; bw.Write(t.X * uvS.X); bw.Write(t.Y * uvS.Y); }
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
        if (o is Model m)
        {
            // Every placement is a gameplay marker; only level art ships as geometry.
            if (Matrix4x4.Decompose(world, out var sc, out var rot, out var tr))
            {
                var yaw = 2.0 * Math.Atan2(
                    rot.W * rot.Z + rot.X * rot.Y,
                    1.0 - 2.0 * (rot.Y * rot.Y + rot.Z * rot.Z));
                var rec = new { group = gp, model = m.Name, position = new[] { tr.X, tr.Y, tr.Z }, facing = (float)yaw };
                markers.Add(rec);
                if (gp.Split('/').Last().StartsWith("mp_spawn_point"))
                    starts.Add(rec);
            }
            if (ShipGroup(gp))
                DumpModel(m, world, gp, $"{m.Name}#{n}");
        }
        n++;
    }
    foreach (var c in g.ChildGroups) Walk(c, gp);
}
Walk(scene.RootNode, null);

var gameplay = new
{
    map = cache.FileName,
    scenario = scnrItem.TagName,
    player_starts = starts,
    spawn_markers = starts,
    markers = markers,
    netgame_flags = new List<object>(),
    netgame_equipment = new List<object>(),
    vehicles = new List<object>(),
    weapons = new List<object>(),
    scenery = new List<object>(),
};

var jo = new JsonSerializerOptions { WriteIndented = true, IncludeFields = true };
File.WriteAllText(Path.Combine(outDir, "parts.json"), JsonSerializer.Serialize(parts, jo));
File.WriteAllText(Path.Combine(outDir, "materials.json"), JsonSerializer.Serialize(materials, jo));
File.WriteAllText(Path.Combine(outDir, "shaders.json"), JsonSerializer.Serialize(new Dictionary<string, object>(), jo));
File.WriteAllText(Path.Combine(outDir, "gameplay.json"), JsonSerializer.Serialize(gameplay, jo));
Console.WriteLine($"parts={parts.Count} materials={materials.Count} textures={textures.Count} starts={starts.Count} markers={markers.Count}");
