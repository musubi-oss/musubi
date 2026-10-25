// Headless World Partition map for tools/nitl.sh (NITL_WP=1): an empty partitioned level whose runtime grid matches the
// Musubi grid, with one tagged marker actor per cell along x ("MusubiWpMarker", "x,y") so a server log shows which cells
// the engine streamed in for the simulator's leases. Editor binary only (the NITL servers run UnrealEditor -server).
#pragma once

#include "CoreMinimal.h"
#include "Commandlets/Commandlet.h"
#include "MusubiNitlWpMapCommandlet.generated.h"

UCLASS()
class UMusubiNitlWpMapCommandlet : public UCommandlet
{
	GENERATED_BODY()
public:
	UMusubiNitlWpMapCommandlet();
	/** -Map=/Game/NitlWP -Cell=256 -Markers=12 (cells 0,0 … 11,0) */
	virtual int32 Main(const FString& Params) override;
};
