#include "MusubiNitlWpMapCommandlet.h"
#include "Components/SceneComponent.h"
#include "Engine/Level.h"
#include "Engine/World.h"
#include "GameFramework/Actor.h"
#include "GameFramework/WorldSettings.h"
#include "HAL/FileManager.h"
#include "Misc/ConfigCacheIni.h"
#include "Misc/FileHelper.h"
#include "Misc/PackageName.h"
#include "Misc/Parse.h"
#include "Misc/Paths.h"
#include "UObject/Package.h"
#include "WorldPartition/WorldPartition.h"
#include "WorldPartition/WorldPartitionRuntimeHash.h"
#include "WorldPartition/WorldPartitionRuntimeSpatialHash.h"
#include "MusubiNitlLog.h"
#if WITH_EDITOR
#include "UObject/SavePackage.h"
#endif

UMusubiNitlWpMapCommandlet::UMusubiNitlWpMapCommandlet()
{
	IsClient = false;
	IsServer = false;
	IsEditor = true;
	LogToConsole = true;
}

int32 UMusubiNitlWpMapCommandlet::Main(const FString& Params)
{
#if !WITH_EDITOR
	UE_LOG(LogMusubiNitl, Error, TEXT("wpmap: needs an editor build (external actors, world partition setup)"));
	return 1;
#else
	FString MapPath = TEXT("/Game/NitlWP");
	int32 Cell = 256, Markers = 6;
	FParse::Value(*Params, TEXT("Map="), MapPath);
	FParse::Value(*Params, TEXT("Cell="), Cell);
	FParse::Value(*Params, TEXT("Markers="), Markers);
	const FString AssetName = FPackageName::GetLongPackageAssetName(MapPath);

	UPackage* Package = CreatePackage(*MapPath);
	Package->FullyLoad();
	UWorld* World = UWorld::CreateWorld(EWorldType::Editor, false, *AssetName, Package);
	if (!World) { UE_LOG(LogMusubiNitl, Error, TEXT("wpmap: CreateWorld failed")); return 1; }
	World->SetFlags(RF_Public | RF_Standalone);
	ULevel* Level = World->PersistentLevel;
	Level->SetUseExternalActors(true);

	// the same runtime grid as the Musubi grid (one WP cell per Musubi cell), the way WorldPartitionConvertCommandlet
	// takes the grid from a per-level ini (Grids is a Config property): LoadConfig from a file we write
	// the spatial hash (one 2D grid with an explicit cell size) rather than the project default hash set, whose partitions are
	// not a plain Grids array
	UWorldPartition* WP = UWorldPartition::CreateOrRepairWorldPartition(World->GetWorldSettings(), {}, UWorldPartitionRuntimeSpatialHash::StaticClass());
	if (!WP || !WP->RuntimeHash) { UE_LOG(LogMusubiNitl, Error, TEXT("wpmap: no world partition / runtime hash")); return 1; }
	const FString Ini = FPaths::ProjectSavedDir() / TEXT("NitlWP.ini");
	FFileHelper::SaveStringToFile(FString::Printf(TEXT("[%s]\nGrids=(GridName=\"MainGrid\",CellSize=%d,LoadingRange=%d,bBlockOnSlowStreaming=False,Origin=(X=0,Y=0),Priority=0)\n"),
		*WP->RuntimeHash->GetClass()->GetPathName(), Cell, Cell * 2), *Ini);
	WP->RuntimeHash->LoadConfig(WP->RuntimeHash->GetClass(), *Ini);

	TArray<UPackage*> ToSave{ Package };
	for (int32 i = 0; i < Markers; ++i)
	{
		FActorSpawnParameters P;
		P.Name = *FString::Printf(TEXT("MusubiWpMarker_%d_0"), i);
		AActor* A = World->SpawnActor<AActor>(FVector((i + 0.5) * Cell, 0.5 * Cell, 0.0), FRotator::ZeroRotator, P);
		if (!A) { UE_LOG(LogMusubiNitl, Error, TEXT("wpmap: spawn %d failed"), i); return 1; }
		USceneComponent* Root = NewObject<USceneComponent>(A, TEXT("Root"));
		A->SetRootComponent(Root);
		A->AddInstanceComponent(Root);
		Root->RegisterComponent();
		A->SetActorLocation(FVector((i + 0.5) * Cell, 0.5 * Cell, 0.0));
		A->Tags = { TEXT("MusubiWpMarker"), *FString::Printf(TEXT("%d,0"), i) };
		A->SetActorLabel(P.Name.ToString());
		A->SetIsSpatiallyLoaded(true);
		if (!A->IsPackageExternal()) A->SetPackageExternal(true);
	}
	// every external actor package, not only the markers: CreateOrRepairWorldPartition spawned AWorldDataLayers (and the
	// minimap actor) into the level — without them on disk a -server/-game load recreates WorldDataLayers, which needs GEditor
	for (AActor* A : Level->Actors)
	{
		if (A && A->IsPackageExternal()) ToSave.AddUnique(A->GetExternalPackage());
	}

	for (UPackage* Pkg : ToSave)
	{
		const bool bMap = Pkg == Package;
		const FString File = FPackageName::LongPackageNameToFilename(Pkg->GetName(), bMap ? FPackageName::GetMapPackageExtension() : FPackageName::GetAssetPackageExtension());
		FSavePackageArgs Args;
		Args.TopLevelFlags = RF_Public | RF_Standalone;
		if (!UPackage::SavePackage(Pkg, bMap ? (UObject*)World : nullptr, *File, Args))
		{
			UE_LOG(LogMusubiNitl, Error, TEXT("wpmap: save %s failed"), *File);
			return 1;
		}
	}
	UE_LOG(LogMusubiNitl, Display, TEXT("wpmap: %s saved, grid cell %d, %d markers (%d packages)"), *MapPath, Cell, Markers, ToSave.Num());
	World->DestroyWorld(false); // CreateWorld initialized it (subsystems incl. WorldPartitionSubsystem): tear it down before exit GC
	return 0;
#endif
}
