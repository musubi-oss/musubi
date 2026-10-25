// Level instances per Musubi cell: one map asset per cell ("x,y" → asset path, "*" = default), loaded into the running
// persistent world with ULevelStreamingDynamic (no travel). The same streamer serves a simulator (wants its leased
// cells + the ring around them, where its ghosts stand and where the next lease lands) and a client (wants the cells
// around its avatar / view), so every server and client of a world runs the same persistent map and a handover or a
// rehome never changes the map.
#pragma once
#include "CoreMinimal.h"
#include "UObject/SoftObjectPtr.h"

class ULevelStreamingDynamic;
class UWorld;

class MUSUBISERVER_API FMusubiCellLevelStreamer
{
public:
	/** Levels from the node (`WorldConfig::levels`); replaces an earlier map. */
	void SetLevels(const TMap<FString, FString>& InLevels);
	/** Levels from `UMusubiSimulatorSettings::CellLevels` — used only while the node sent none. */
	void SetFallbackLevels(const TMap<FString, TSoftObjectPtr<UWorld>>& InLevels);
	void SetGrid(double InCellSize, float InStride) { CellSize = InCellSize; Stride = InStride; }

	bool HasLevels() const { return Levels.Num() > 0 || Fallback.Num() > 0; }
	double GetCellSize() const { return CellSize; }
	/** Asset path for a cell: its own entry, else "*", else empty (nothing to stream there). */
	FString LevelFor(const FIntPoint& Cell) const;

	/** Cells whose squares intersect the disc (centre, radius) — the client's wanted set. */
	TSet<FIntPoint> CellsAround(const FVector& Centre, double Radius) const;
	/** The cells plus their 8 neighbours — the simulator's wanted set. */
	static TSet<FIntPoint> WithRing(const TSet<FIntPoint>& Cells);

	/** Load every wanted cell that has a level and is not loaded; unload the loaded ones no longer wanted. */
	void Want(UWorld* World, const TSet<FIntPoint>& Cells, const TCHAR* Why);
	void UnloadAll();

	ULevelStreamingDynamic* Instance(const FIntPoint& Cell) const;
	TArray<FIntPoint> LoadedCells() const;
	static FString Key(const FIntPoint& Cell) { return FString::Printf(TEXT("%d,%d"), Cell.X, Cell.Y); }

private:
	void Unload(const FIntPoint& Cell, const TCHAR* Why);

	TMap<FString, FString> Levels, Fallback;
	double CellSize = 0;
	float Stride = 0.f;
	TMap<FIntPoint, TWeakObjectPtr<ULevelStreamingDynamic>> Instances;
};
