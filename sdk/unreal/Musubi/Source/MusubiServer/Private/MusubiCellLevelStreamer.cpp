#include "MusubiCellLevelStreamer.h"
#include "Engine/LevelStreamingDynamic.h"
#include "Engine/World.h"
#include "MusubiServerModule.h"

void FMusubiCellLevelStreamer::SetLevels(const TMap<FString, FString>& InLevels)
{
	Levels = InLevels;
}

void FMusubiCellLevelStreamer::SetFallbackLevels(const TMap<FString, TSoftObjectPtr<UWorld>>& InLevels)
{
	Fallback.Reset();
	for (const auto& KV : InLevels) if (!KV.Value.IsNull()) Fallback.Add(KV.Key, KV.Value.GetLongPackageName());
}

FString FMusubiCellLevelStreamer::LevelFor(const FIntPoint& Cell) const
{
	const TMap<FString, FString>& M = Levels.Num() > 0 ? Levels : Fallback;
	if (const FString* P = M.Find(Key(Cell))) return *P;
	if (const FString* P = M.Find(TEXT("*"))) return *P;
	return FString();
}

TSet<FIntPoint> FMusubiCellLevelStreamer::CellsAround(const FVector& Centre, double Radius) const
{
	TSet<FIntPoint> Out;
	if (CellSize <= 0) return Out;
	const int32 X0 = FMath::FloorToInt((Centre.X - Radius) / CellSize), X1 = FMath::FloorToInt((Centre.X + Radius) / CellSize);
	const int32 Y0 = FMath::FloorToInt((Centre.Y - Radius) / CellSize), Y1 = FMath::FloorToInt((Centre.Y + Radius) / CellSize);
	for (int32 X = X0; X <= X1; ++X)
		for (int32 Y = Y0; Y <= Y1; ++Y)
		{
			// nearest point of the cell's square to the centre
			const double NX = FMath::Clamp(Centre.X, X * CellSize, (X + 1) * CellSize), NY = FMath::Clamp(Centre.Y, Y * CellSize, (Y + 1) * CellSize);
			if (FMath::Square(NX - Centre.X) + FMath::Square(NY - Centre.Y) <= Radius * Radius) Out.Add(FIntPoint(X, Y));
		}
	return Out;
}

TSet<FIntPoint> FMusubiCellLevelStreamer::WithRing(const TSet<FIntPoint>& Cells)
{
	TSet<FIntPoint> Out = Cells;
	for (const FIntPoint& C : Cells)
		for (int32 DX = -1; DX <= 1; ++DX)
			for (int32 DY = -1; DY <= 1; ++DY) Out.Add(FIntPoint(C.X + DX, C.Y + DY));
	return Out;
}

void FMusubiCellLevelStreamer::Want(UWorld* World, const TSet<FIntPoint>& Cells, const TCHAR* Why)
{
	if (!World) return;
	TArray<FIntPoint> Gone;
	for (const auto& KV : Instances) if (!Cells.Contains(KV.Key)) Gone.Add(KV.Key);
	for (const FIntPoint& C : Gone) Unload(C, Why);
	for (const FIntPoint& C : Cells)
	{
		if (Instances.Contains(C)) continue;
		const FString Path = LevelFor(C);
		if (Path.IsEmpty()) continue;
		const FVector At = Stride > 0.f ? FVector((double)C.X * Stride, (double)C.Y * Stride, 0.0) : FVector::ZeroVector;
		bool bOk = false;
		// same signature on 5.3 … 5.8 (Engine/LevelStreamingDynamic.h); the name override keeps one instance per cell
		ULevelStreamingDynamic* LS = ULevelStreamingDynamic::LoadLevelInstance(World, Path, At, FRotator::ZeroRotator, bOk, FString::Printf(TEXT("MusubiCell_%d_%d"), C.X, C.Y));
		if (!bOk || !LS)
		{
			UE_LOG(LogMusubiServer, Warning, TEXT("cell level %s: %s failed to load"), *Key(C), *Path);
			continue;
		}
		Instances.Add(C, LS);
		UE_LOG(LogMusubiServer, Log, TEXT("cell level %s loaded (%s) [%s]"), *Key(C), *Path, Why);
	}
}

void FMusubiCellLevelStreamer::Unload(const FIntPoint& Cell, const TCHAR* Why)
{
	TWeakObjectPtr<ULevelStreamingDynamic> W;
	Instances.RemoveAndCopyValue(Cell, W);
	if (ULevelStreamingDynamic* LS = W.Get())
	{
		LS->SetShouldBeLoaded(false);
		LS->SetShouldBeVisible(false);
		LS->SetIsRequestingUnloadAndRemoval(true);
	}
	UE_LOG(LogMusubiServer, Log, TEXT("cell level %s unloaded [%s]"), *Key(Cell), Why);
}

void FMusubiCellLevelStreamer::UnloadAll()
{
	TArray<FIntPoint> All;
	Instances.GetKeys(All);
	for (const FIntPoint& C : All) Unload(C, TEXT("shutdown"));
}

ULevelStreamingDynamic* FMusubiCellLevelStreamer::Instance(const FIntPoint& Cell) const
{
	const TWeakObjectPtr<ULevelStreamingDynamic>* W = Instances.Find(Cell);
	return W ? W->Get() : nullptr;
}

TArray<FIntPoint> FMusubiCellLevelStreamer::LoadedCells() const
{
	TArray<FIntPoint> Out;
	Instances.GetKeys(Out);
	return Out;
}
