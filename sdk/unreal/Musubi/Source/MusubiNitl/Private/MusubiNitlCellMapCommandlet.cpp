#include "MusubiNitlCellMapCommandlet.h"
#include "Components/SceneComponent.h"
#include "Engine/Level.h"
#include "Engine/World.h"
#include "GameFramework/Actor.h"
#include "Misc/PackageName.h"
#include "Misc/Parse.h"
#include "UObject/Package.h"
#include "MusubiNitlLog.h"
#if WITH_EDITOR
#include "UObject/SavePackage.h"
#endif

UMusubiNitlCellMapCommandlet::UMusubiNitlCellMapCommandlet()
{
	IsClient = false;
	IsServer = false;
	IsEditor = true;
	LogToConsole = true;
}

int32 UMusubiNitlCellMapCommandlet::Main(const FString& Params)
{
#if !WITH_EDITOR
	UE_LOG(LogMusubiNitl, Error, TEXT("cellmap: needs an editor build"));
	return 1;
#else
	FString Prefix = TEXT("/Game/NitlCell");
	int32 Cell = 256, Count = 12;
	FParse::Value(*Params, TEXT("Prefix="), Prefix);
	FParse::Value(*Params, TEXT("Cell="), Cell);
	FParse::Value(*Params, TEXT("Count="), Count);
	for (int32 i = 0; i < Count; ++i)
	{
		const FString MapPath = FString::Printf(TEXT("%s_%d_0"), *Prefix, i);
		const FString AssetName = FPackageName::GetLongPackageAssetName(MapPath);
		UPackage* Package = CreatePackage(*MapPath);
		Package->FullyLoad();
		UWorld* World = UWorld::CreateWorld(EWorldType::Editor, false, *AssetName, Package);
		if (!World) { UE_LOG(LogMusubiNitl, Error, TEXT("cellmap: CreateWorld %s failed"), *MapPath); return 1; }
		World->SetFlags(RF_Public | RF_Standalone);
		FActorSpawnParameters P;
		P.Name = *FString::Printf(TEXT("MusubiCellMarker_%d_0"), i);
		const FVector At((i + 0.5) * Cell, 0.5 * Cell, 0.0);
		AActor* A = World->SpawnActor<AActor>(At, FRotator::ZeroRotator, P);
		if (!A) { UE_LOG(LogMusubiNitl, Error, TEXT("cellmap: spawn %d failed"), i); return 1; }
		USceneComponent* Root = NewObject<USceneComponent>(A, TEXT("Root"));
		A->SetRootComponent(Root);
		A->AddInstanceComponent(Root);
		Root->RegisterComponent();
		A->SetActorLocation(At);
		A->Tags = { TEXT("MusubiCellLevel"), *FString::Printf(TEXT("%d,0"), i) };
		A->SetActorLabel(P.Name.ToString());
		const FString File = FPackageName::LongPackageNameToFilename(MapPath, FPackageName::GetMapPackageExtension());
		FSavePackageArgs Args;
		Args.TopLevelFlags = RF_Public | RF_Standalone;
		if (!UPackage::SavePackage(Package, World, *File, Args)) { UE_LOG(LogMusubiNitl, Error, TEXT("cellmap: save %s failed"), *File); return 1; }
		World->DestroyWorld(false);
	}
	UE_LOG(LogMusubiNitl, Display, TEXT("cellmap: %d levels saved under %s_<i>_0, cell %d"), Count, *Prefix, Cell);
	return 0;
#endif
}
