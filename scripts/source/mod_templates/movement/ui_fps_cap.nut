global function UICodeCallback___ID___ModInit

// Air movement is frame-time sensitive; above this rate the client's prediction
// drifts from the server.
const int __NS_UPPER___FPS_CAP = 300

struct
{
	bool capped = false
	int playerFpsMax = 0
} file

void function UICodeCallback___ID___ModInit()
{
	AddUICallback_LevelLoadingFinished( __NS____OnLevelLoaded )
	AddUICallback_LevelShutdown( __NS____RestoreFpsMax )
	AddUICallback_UIShutdown( __NS____RestoreFpsMax )
	__NS____OnLevelLoaded()
}

void function __NS____OnLevelLoaded()
{
	if ( GetActiveLevel() != "__MAP__" )
	{
		__NS____RestoreFpsMax()
		return
	}
	if ( file.capped )
		return

	int fpsMax = GetConVarInt( "fps_max" )
	if ( fpsMax > 0 && fpsMax <= __NS_UPPER___FPS_CAP )
		return

	file.playerFpsMax = fpsMax
	file.capped = true
	SetConVarInt( "fps_max", __NS_UPPER___FPS_CAP )
}

// Leaves the player's value alone if they changed it while on the map.
void function __NS____RestoreFpsMax()
{
	if ( !file.capped )
		return

	file.capped = false
	if ( GetConVarInt( "fps_max" ) == __NS_UPPER___FPS_CAP )
		SetConVarInt( "fps_max", file.playerFpsMax )
}
