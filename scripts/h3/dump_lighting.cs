// Run from the scenario's edit context: CS < dump_lighting.cs <out dir>
var scn = Definition as TagTool.Tags.Definitions.Scenario;
string outDir = Args[0];
System.IO.Directory.CreateDirectory(outDir);
var sb = new System.Text.StringBuilder();
using (var stream = Cache.OpenCacheRead())
{
    var sbspRef = scn.StructureBsps[0];
    Console.WriteLine("sbsp");
    var sbsp = Cache.Deserialize<TagTool.Tags.Definitions.ScenarioStructureBsp>(stream, sbspRef.StructureBsp);
    TagTool.Cache.CachedTag lbspTag = null;
    foreach (var t in Cache.TagCache.NonNull())
        if (t.IsInGroup("Lbsp")) lbspTag = t;
    Console.WriteLine("lbsp " + lbspTag);
    var lbsp = Cache.Deserialize<TagTool.Tags.Definitions.ScenarioLightmapBspData>(stream, lbspTag);
    Console.WriteLine("scales");
    for (int i = 0; i < lbsp.CoefficientsMapScale.Length; i++)
        sb.AppendLine("SCALE\t" + i + "\t" + lbsp.CoefficientsMapScale[i].Scale);
    string H(ushort[] a) { var s = new System.Text.StringBuilder(); foreach (var v in a) s.Append(v).Append(','); return s.ToString(); }
    string P(TagTool.Common.HalfRGBLightProbe p) => H(p.DominantLightDirection) + "\t" + H(p.DominantLightIntensity) + "\t" + H(p.SHRed) + "\t" + H(p.SHGreen) + "\t" + H(p.SHBlue);
    for (int i = 0; i < lbsp.InstancedGeometryLightProbes.Count; i++)
    {
        string pos = "";
        if (i < sbsp.InstancedGeometryInstances.Count)
        {
            var m = sbsp.InstancedGeometryInstances[i].Matrix;
            pos = m.m41 + "," + m.m42 + "," + m.m43;
        }
        sb.AppendLine("IGP\t" + i + "\t" + pos + "\t" + P(lbsp.InstancedGeometryLightProbes[i].LightProbe));
    }
    Console.WriteLine("scenery");
    var byHandle = new System.Collections.Generic.Dictionary<uint, string>();
    for (int i = 0; i < scn.Scenery.Count; i++)
    {
        dynamic e = scn.Scenery[i];
        try { byHandle[(uint)e.ObjectId.UniqueHandle.Value] = e.Position.X + "," + e.Position.Y + "," + e.Position.Z; } catch { }
    }
    foreach (var p in lbsp.SceneryLightProbes)
    {
        string pos = "";
        byHandle.TryGetValue((uint)p.ObjectId.UniqueHandle.Value, out pos);
        sb.AppendLine("SCP\t" + p.ObjectId.UniqueHandle.Value + "\t" + pos + "\t" + P(p.LightProbe));
    }
    foreach (var pair in new[] { ("sh", lbsp.LightmapSHCoefficientsBitmap), ("intensity", lbsp.LightmapDominantLightDirectionBitmap) })
    {
        Console.WriteLine("bitmap " + pair.Item1);
        var bm = Cache.Deserialize<TagTool.Tags.Definitions.Bitmap>(stream, pair.Item2);
        var img = bm.Images[0];
        sb.AppendLine("BITMAP\t" + pair.Item1 + "\t" + img.Width + "x" + img.Height + "x" + img.Depth + "\t" + img.Format + "\t" + img.Type);
        var bb = TagTool.Bitmaps.BitmapExtractor.ExtractBitmap(Cache, bm, 0, pair.Item2.Name, true);
        System.IO.File.WriteAllBytes(System.IO.Path.Combine(outDir, pair.Item1 + ".bin"), bb.Data);
        sb.AppendLine("RAW\t" + pair.Item1 + "\t" + bb.Width + "x" + bb.Height + "x" + bb.Depth + "\t" + bb.Format + "\t" + bb.Data.Length);
    }
}
System.IO.File.WriteAllText(System.IO.Path.Combine(outDir, "lighting.tsv"), sb.ToString());
Console.WriteLine("dump_lighting done");
