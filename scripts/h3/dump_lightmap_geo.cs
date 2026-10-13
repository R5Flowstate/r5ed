// Run from the scenario's edit context: CS < dump_lightmap_geo.cs <out file>
// Writes every per-pixel-lit BSP cluster triangle (mesh index, then per corner
// position xyz, normal xyz, lightmap uv) as float32 records, so the map's baked
// lighting can be sampled at any surface point.
TagTool.Cache.CachedTag lt = null;
foreach (var t in Cache.TagCache.NonNull()) if (t.IsInGroup("Lbsp")) lt = t;
using (var stream = Cache.OpenCacheRead())
using (var w = new System.IO.BinaryWriter(System.IO.File.Create(Args[0])))
{
    var lbsp = Cache.Deserialize<TagTool.Tags.Definitions.ScenarioLightmapBspData>(stream, lt);
    var g = lbsp.Geometry;
    var def = Cache.ResourceCache.GetRenderGeometryApiResourceDefinition(g.Resource);
    TagTool.Geometry.IVertexStream Open(TagTool.Geometry.VertexBufferDefinition d) =>
        TagTool.Geometry.VertexStreamFactory.Create(Cache.Version, Cache.Platform, new System.IO.MemoryStream(d.Data.Data));
    int tris = 0, meshes = 0;
    for (int mi = 0; mi < g.Meshes.Count; mi++)
    {
        var mesh = g.Meshes[mi];
        int v0 = mesh.VertexBufferIndices[0], v1 = mesh.VertexBufferIndices[1], i0 = mesh.IndexBufferIndices[0];
        if (v0 < 0 || v1 < 0 || i0 < 0) continue;
        var d0 = def.VertexBuffers[v0].Definition;
        var d1 = def.VertexBuffers[v1].Definition;
        if (d0.Format != TagTool.Geometry.VertexBufferFormat.World || d1.Format != TagTool.Geometry.VertexBufferFormat.StaticPerPixel) continue;
        int n = d0.Count;
        var pos = new TagTool.Common.RealQuaternion[n];
        var nrm = new TagTool.Common.RealVector3d[n];
        var vs = Open(d0);
        for (int i = 0; i < n; i++) { var v = vs.ReadWorldVertex(); pos[i] = v.Position; nrm[i] = v.Normal; }
        var uv = new TagTool.Common.RealVector2d[n];
        var ls = Open(d1);
        for (int i = 0; i < n; i++) uv[i] = ls.ReadStaticPerPixelData().Texcoord;
        var ib = def.IndexBuffers[i0].Definition;
        foreach (var part in mesh.Parts)
        {
            var istream = new TagTool.Geometry.IndexBufferStream(new System.IO.MemoryStream(ib.Data.Data), Cache.Endianness);
            istream.Position = part.FirstIndex;
            ushort[] idx = ib.Format == TagTool.Geometry.IndexBufferFormat.TriangleStrip
                ? istream.ReadTriangleStrip(part.IndexCount) : istream.ReadIndices(part.IndexCount);
            for (int k = 0; k + 2 < idx.Length; k += 3)
            {
                int a = idx[k], b = idx[k + 1], c = idx[k + 2];
                if (a == b || b == c || a == c || a >= n || b >= n || c >= n) continue;
                w.Write((float)mi);
                foreach (var j in new[] { a, b, c })
                {
                    w.Write(pos[j].I); w.Write(pos[j].J); w.Write(pos[j].K);
                    w.Write(nrm[j].I); w.Write(nrm[j].J); w.Write(nrm[j].K);
                    w.Write(uv[j].I); w.Write(uv[j].J);
                }
                tris++;
            }
        }
        meshes++;
    }
    Console.WriteLine("lightmap geo: " + meshes + " meshes, " + tris + " triangles");
}
