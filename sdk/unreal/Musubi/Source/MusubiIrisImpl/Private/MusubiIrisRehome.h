#pragma once
#include "CoreMinimal.h"
#include "Engine/World.h"
#include "GameFramework/Actor.h"
#include "MusubiAuthorityComponent.h"
#include "MusubiEntityRegistry.h"
#include "MusubiIrisModule.h"

// A client in the middle of an in-place rehome keeps its entity actors and the player's controller/state: the engine
// detaches them with the old connection, the new owner re-attaches to the same objects through the Musubi factories.
inline bool MusubiIsKeptAcrossRehome(const AActor* A, bool bDetaching = false)
{
	UWorld* World = A ? A->GetWorld() : nullptr;
	if (!World || World->GetNetMode() != NM_Client) return false;
	const UMusubiEntityRegistry* Reg = UMusubiEntityRegistry::Get(World);
	if (!Reg || !(bDetaching ? Reg->IsKeepingOnDetach() : Reg->IsRehoming())) return false;
	if (Reg->IsRehomeKeptPlayerActor(A)) return true;
	const UMusubiAuthorityComponent* Auth = A->FindComponentByClass<UMusubiAuthorityComponent>();
	return Auth && !Auth->NetEntityId.IsEmpty();
}

inline bool MusubiKeepActorOnDetach(UObject* Instance)
{
	AActor* A = Cast<AActor>(Instance);
	if (!MusubiIsKeptAcrossRehome(A, /*bDetaching*/ true)) return false;
	if (UMusubiEntityRegistry* Reg = UMusubiEntityRegistry::Get(A->GetWorld())) Reg->NoteIrisKept();
	UE_LOG(LogMusubiIris, Log, TEXT("rehome: keeping %s for the new owner"), *A->GetName());
	return true;
}

inline bool MusubiKeepSubObjectOnDetach(UObject* Instance)
{
	const AActor* Owner = Instance ? Instance->GetTypedOuter<AActor>() : nullptr;
	return MusubiIsKeptAcrossRehome(Owner, /*bDetaching*/ true);
}
