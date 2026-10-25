// Headless per-cell levels for tools/nitl.sh (NITL_LEVELS=1): one small map per Musubi cell along x, each with a marker
// actor at the cell's centre tagged ("MusubiCellLevel", "x,0"), placed in world space (no stride). The world's
// `levels` config points cell "i,0" at /Game/NitlCell_i_0; servers and clients stream them as level instances.
#pragma once
#include "CoreMinimal.h"
#include "Commandlets/Commandlet.h"
#include "MusubiNitlCellMapCommandlet.generated.h"

UCLASS()
class UMusubiNitlCellMapCommandlet : public UCommandlet
{
	GENERATED_BODY()
public:
	UMusubiNitlCellMapCommandlet();
	/** -Prefix=/Game/NitlCell -Cell=256 -Count=12 (cells 0,0 … 11,0) */
	virtual int32 Main(const FString& Params) override;
};
