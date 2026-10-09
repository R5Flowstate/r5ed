var scn = Definition as TagTool.Tags.Definitions.Scenario;
string outPath = "gameplay_tags.tsv";
try { if (Args != null && Args.Count > 0 && Args[0] != null) outPath = Args[0]; } catch { }
var sb = new System.Text.StringBuilder();
System.Func<object, string> S = o => (o == null ? "" : ("" + o)).Replace("\t", " ");
void Pal(string block, System.Collections.IList list)
{
    if (list == null) return;
    for (int i = 0; i < list.Count; i++)
    {
        try
        {
            var e = (TagTool.Tags.Definitions.Scenario.ScenarioPaletteEntry)list[i];
            sb.AppendLine("PAL\t" + block + "\t" + i + "\t" + S(e.Object));
        }
        catch (System.Exception ex) { sb.AppendLine("ERR\t" + block + "\t" + i + "\t" + S(ex.Message)); }
    }
}
void Inst(string block, System.Collections.IList list, System.Collections.IList pal)
{
    if (list == null) return;
    for (int i = 0; i < list.Count; i++)
    {
        try
        {
            dynamic e = list[i];
            string tag = "";
            try { int pi = (int)e.PaletteIndex; tag = S(((TagTool.Tags.Definitions.Scenario.ScenarioPaletteEntry)pal[pi]).Object); } catch { }
            string team = "", st = "";
            try { team = S(e.Multiplayer.Team); st = S(e.Multiplayer.SpawnTime); } catch { }
            float yaw = 0, pitch = 0, roll = 0;
            try { yaw = (float)e.Rotation.Yaw.Degrees; pitch = (float)e.Rotation.Pitch.Degrees; roll = (float)e.Rotation.Roll.Degrees; } catch { }
            float x = 0, y = 0, z = 0;
            try { x = (float)e.Position.X; y = (float)e.Position.Y; z = (float)e.Position.Z; } catch { }
            sb.AppendLine("INST\t" + block + "\t" + ((int)e.PaletteIndex) + "\t" + tag + "\t" + x + "\t" + y + "\t" + z + "\t" + yaw + "\t" + pitch + "\t" + roll + "\t" + st + "\t" + team);
        }
        catch (System.Exception ex) { sb.AppendLine("ERR\t" + block + "\t" + i + "\t" + S(ex.Message)); }
    }
}
Pal("WeaponPalette", scn.WeaponPalette);
Pal("VehiclePalette", scn.VehiclePalette);
Pal("EquipmentPalette", scn.EquipmentPalette);
Pal("SceneryPalette", scn.SceneryPalette);
Pal("CratePalette", scn.CratePalette);
Inst("Weapons", scn.Weapons, scn.WeaponPalette);
Inst("Vehicles", scn.Vehicles, scn.VehiclePalette);
Inst("Equipment", scn.Equipment, scn.EquipmentPalette);
Inst("Scenery", scn.Scenery, scn.SceneryPalette);
Inst("Crates", scn.Crates, scn.CratePalette);
try
{
    if (scn.PlayerStartingLocations != null)
        foreach (var l in scn.PlayerStartingLocations)
            sb.AppendLine("LOC\t" + l.Position.X + "\t" + l.Position.Y + "\t" + l.Position.Z + "\t" + l.Facing.Yaw.Degrees);
}
catch (System.Exception ex) { sb.AppendLine("ERR\tLOC\t" + S(ex.Message)); }
try
{
    if (scn.TriggerVolumes != null)
        foreach (var t in scn.TriggerVolumes)
            sb.AppendLine("TRIG\t" + S(t.Name) + "\t" + t.Position.X + "\t" + t.Position.Y + "\t" + t.Position.Z + "\t" + t.Extents.X + "\t" + t.Extents.Y + "\t" + t.Extents.Z + "\t" + t.KillVolume);
}
catch (System.Exception ex) { sb.AppendLine("ERR\tTRIG\t" + S(ex.Message)); }
try
{
    if (scn.SoftCeilings != null)
        foreach (var c in scn.SoftCeilings)
            sb.AppendLine("SOFT\t" + S(c.Name) + "\t" + S(c.Type));
}
catch (System.Exception ex) { sb.AppendLine("ERR\tSOFT\t" + S(ex.Message)); }
string[] sands = new string[] { "SandboxSpawning", "SandboxGoalObjects", "SandboxTeleporters", "SandboxWeapons", "SandboxVehicles", "SandboxEquipment", "SandboxScenery" };
foreach (var b in sands)
{
    try
    {
        var list = (System.Collections.IList)scn.GetType().GetField(b).GetValue(scn);
        if (list == null) continue;
        for (int i = 0; i < list.Count; i++)
        {
            dynamic e = list[i];
            sb.AppendLine("SAND\t" + b + "\t" + i + "\t" + S(e.Object) + "\t" + S(e.Name));
        }
    }
    catch (System.Exception ex) { sb.AppendLine("ERR\t" + b + "\t" + S(ex.Message)); }
}
System.IO.File.WriteAllText(outPath, sb.ToString());
System.Console.WriteLine("wrote " + outPath);
