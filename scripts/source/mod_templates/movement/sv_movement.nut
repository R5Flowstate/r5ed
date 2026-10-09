global function CodeCallback___ID___ModInit

void function CodeCallback___ID___ModInit()
{
	if ( !__NS____IsModMap() )
		return

	if ( GetCurrentPlaylistVarBool( "mapmod_invincible", __INVINCIBLE__ ) )
		AddDamageCallback( "player", __NS____NoDamage )

	__NS____RegisterPushVolumes()
	AddCallback_OnPlayerRespawned( __NS____OnPlayerRespawned )
	AddTriggerEditorClassFunc( "trigger_source_teleport", __NS____TeleportTriggerInit )
}

vector function __NS____ParseVector( string text )
{
	array<string> parts = split( text, " " )
	if ( parts.len() != 3 )
		return <0, 0, 0>
	return < parts[0].tofloat(), parts[1].tofloat(), parts[2].tofloat() >
}

void function __NS____TriggerForAllPlayers( entity trigger )
{
	trigger.kv.triggerFilterUseNew = 1
	trigger.kv.triggerFilterPlayer = "all"
	trigger.kv.triggerFilterPhaseShift = "any"
	trigger.kv.triggerFilterNpc = "none"
	trigger.kv.triggerFilterNonCharacter = 0
	trigger.kv.triggerFilterTeamMilitia = 1
	trigger.kv.triggerFilterTeamIMC = 1
	trigger.kv.triggerFilterTeamNeutral = 1
	trigger.kv.triggerFilterTeamBeast = 1
	trigger.kv.triggerFilterTeamOther = 1
}

// Source teleports stop the player and face them the way the destination faces.
void function __NS____TeleportTriggerInit( entity trigger )
{
	if ( !trigger.HasKey( "teleport_origin" ) )
	{
		Warning( "[MAPMOD] teleport trigger at " + trigger.GetOrigin() + " has no destination" )
		return
	}
	__NS____TriggerForAllPlayers( trigger )
	vector origin = __NS____ParseVector( expect string( trigger.kv.teleport_origin ) )
	vector angles = __NS____ParseVector( expect string( trigger.kv.teleport_angles ) )
	printt( "[MAPMOD] teleport trigger at " + trigger.GetOrigin() + " -> " + origin )
	trigger.SetEnterCallback( void function( entity trigger, entity ent ) : ( origin, angles ) {
		if ( !IsValid( ent ) || !ent.IsPlayer() || !IsAlive( ent ) )
			return
		printt( "[MAPMOD] teleport " + ent + " -> " + origin )
		ent.SetVelocity( <0, 0, 0> )
		ent.SetOrigin( origin )
		ent.SetAngles( < 0, angles.y, 0 > )
		ent.SnapEyeAngles( angles )
	} )
}

void function __NS____NoDamage( entity victim, var damageInfo )
{
	DamageInfo_SetDamage( damageInfo, 0 )
}

void function __NS____OnPlayerRespawned( entity player )
{
	__NS____ApplyMovement( player )
	array<string> points = split( GetCurrentPlaylistVarString( "mapmod_spawn_points", "" ), ";" )
	if ( points.len() > 0 )
		thread __NS____PlaceAtPoint_THREAD( player, points.getrandom() )
	else
		thread __NS____PlaceAtSpawn_THREAD( player )
	if ( GetCurrentPlaylistVarBool( "mapmod_no_melee", __NO_MELEE__ ) )
		thread __NS____TakeMelee_THREAD( player )
}

// "x y z yaw;x y z yaw": where players enter the map when its own spawnpoints only
// lead through a spawn room of teleports. Those teleports can fire in the first
// ticks, so the placement is checked once more.
void function __NS____PlaceAtPoint_THREAD( entity player, string entry )
{
	player.EndSignal( "OnDestroy" )
	player.EndSignal( "OnDeath" )

	array<string> parts = split( entry, " " )
	if ( parts.len() != 4 )
	{
		Warning( "[MAPMOD] mapmod_spawn_points entry '" + entry + "' is not 'x y z yaw'" )
		return
	}
	float lift = GetCurrentPlaylistVarFloat( "mapmod_spawn_lift", 16.0 )
	vector target = < parts[0].tofloat(), parts[1].tofloat(), parts[2].tofloat() + lift >
	vector angles = < 0, parts[3].tofloat(), 0 >
	for ( int pass = 0; pass < 2; pass++ )
	{
		if ( pass == 0 || Distance( player.GetOrigin(), target ) > 256.0 )
		{
			if ( pass > 0 )
				printt( "[MAPMOD] spawn " + player + " moved off " + target + ", placing again" )
			player.SetVelocity( <0, 0, 0> )
			player.SetOrigin( target )
			player.SetAngles( angles )
			player.SnapEyeAngles( angles )
		}
		wait 0.3
	}
}

// The first spawn of a match can land away from the map's spawnpoints, and some
// spawnpoints sit flush with geometry, so every spawn is settled on one, lifted.
void function __NS____PlaceAtSpawn_THREAD( entity player )
{
	player.EndSignal( "OnDestroy" )
	player.EndSignal( "OnDeath" )
	WaitFrame()

	array<entity> spawns = GetEntArrayByClass_Expensive( "info_spawnpoint_human" )
	if ( spawns.len() == 0 )
		return

	vector at = player.GetOrigin()
	entity spawn = spawns[0]
	foreach ( candidate in spawns )
	{
		if ( DistanceSqr( candidate.GetOrigin(), at ) < DistanceSqr( spawn.GetOrigin(), at ) )
			spawn = candidate
	}
	if ( Distance( spawn.GetOrigin(), at ) > 128.0 )
	{
		spawn = spawns.getrandom()
		printt( "[MAPMOD] spawn " + player + " at " + at + " is off the spawnpoints -> " + spawn.GetOrigin() )
	}

	float lift = GetCurrentPlaylistVarFloat( "mapmod_spawn_lift", 16.0 )
	vector angles = < 0, spawn.GetAngles().y, 0 >
	player.SetVelocity( <0, 0, 0> )
	player.SetOrigin( spawn.GetOrigin() + < 0, 0, lift > )
	player.SetAngles( angles )
	player.SnapEyeAngles( angles )
}

// Loadouts hand melee out after this callback and again on later changes, so
// it is taken whenever it shows up for as long as the player lives.
void function __NS____TakeMelee_THREAD( entity player )
{
	player.EndSignal( "OnDestroy" )
	player.EndSignal( "OnDeath" )
	while ( true )
	{
		if ( IsValid( player.GetOffhandWeapon( OFFHAND_MELEE ) ) )
			player.TakeOffhandWeapon( OFFHAND_MELEE )
		wait 0.5
	}
}
