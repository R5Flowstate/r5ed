global function ClientCodeCallback___ID___ModInit

void function ClientCodeCallback___ID___ModInit()
{
	if ( !__NS____IsModMap() )
		return

	__NS____RegisterPushVolumes()
	AddCallback_LocalClientPlayerSpawned( __NS____ApplyMovement )
	thread __NS____ApplyWhenReady_THREAD()
}

// The first spawn can land before this callback is registered, or while the
// local settings block is not ready yet.
void function __NS____ApplyWhenReady_THREAD()
{
	float deadline = Time() + 30.0
	entity player = GetLocalClientPlayer()
	while ( Time() < deadline && ( !IsValid( player ) || !player.IsPlayer() || !IsAlive( player ) ) )
	{
		wait 0.5
		player = GetLocalClientPlayer()
	}
	if ( !IsValid( player ) || !player.IsPlayer() || !IsAlive( player ) )
		return

	__NS____ApplyMovement( player )
	wait 1.0
	if ( IsValid( player ) && player.IsPlayer() && IsAlive( player ) )
		__NS____ApplyMovement( player )
}
