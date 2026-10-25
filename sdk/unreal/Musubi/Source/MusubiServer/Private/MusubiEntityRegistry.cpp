#include "MusubiEntityRegistry.h"
#include "MusubiAuthorityComponent.h"
#include "Engine/Engine.h"
#include "Engine/World.h"
#include "GameFramework/Actor.h"

DEFINE_LOG_CATEGORY_STATIC(LogMusubiEntity, Log, All);

AActor* FMusubiEntityHandle::Get(const UObject* WorldContext) const
{
	UMusubiEntityRegistry* R = IsSet() ? UMusubiEntityRegistry::Get(WorldContext) : nullptr;
	return R ? R->Resolve(EntityId) : nullptr;
}

const FName UMusubiEntityRegistry::IrisNetFactoryName(TEXT("MusubiActor"));
const FName UMusubiEntityRegistry::IrisSubObjectFactoryName(TEXT("MusubiSubObject"));

UMusubiEntityRegistry* UMusubiEntityRegistry::Get(const UObject* WorldContext)
{
	UWorld* W = GEngine ? GEngine->GetWorldFromContextObject(WorldContext, EGetWorldErrorMode::ReturnNull) : nullptr;
	return W ? W->GetSubsystem<UMusubiEntityRegistry>() : nullptr;
}

AActor* UMusubiEntityRegistry::Resolve(const FString& EntityId) const
{
	const TWeakObjectPtr<AActor>* P = Actors.Find(EntityId);
	AActor* A = P ? P->Get() : nullptr;
	return (A && IsValid(A)) ? A : nullptr;
}

AActor* UMusubiEntityRegistry::ResolveEntity(const UObject* WorldContext, const FString& EntityId)
{
	UMusubiEntityRegistry* R = Get(WorldContext);
	return R ? R->Resolve(EntityId) : nullptr;
}

AActor* UMusubiEntityRegistry::ResolveHandle(const UObject* WorldContext, const FMusubiEntityHandle& Handle)
{
	return Handle.Get(WorldContext);
}

FMusubiEntityHandle UMusubiEntityRegistry::HandleOf(AActor* Actor)
{
	FMusubiEntityHandle H;
	const UMusubiAuthorityComponent* Auth = Actor ? Actor->FindComponentByClass<UMusubiAuthorityComponent>() : nullptr;
	if (Auth) H.EntityId = Auth->NetEntityId.IsEmpty() ? Auth->GetEntityId() : Auth->NetEntityId;
	return H;
}

TArray<FString> UMusubiEntityRegistry::EntityIds() const
{
	TArray<FString> Out;
	for (const auto& KV : Actors)
		if (KV.Value.IsValid()) Out.Add(KV.Key);
	return Out;
}

void UMusubiEntityRegistry::Register(const FString& EntityId, AActor* Actor)
{
	if (EntityId.IsEmpty() || !Actor) return;
	TWeakObjectPtr<AActor>& Slot = Actors.FindOrAdd(EntityId);
	AActor* Old = Slot.Get();
	if (Old == Actor) return;
	Slot = Actor;
	if (Old && IsValid(Old))
	{
		++NumChanges;
		UE_LOG(LogMusubiEntity, Log, TEXT("entity %s: actor %s replaced by %s"), *EntityId, *Old->GetName(), *Actor->GetName());
		OnEntityActorChangedNative.Broadcast(EntityId, Old, Actor);
		OnEntityActorChanged.Broadcast(EntityId, Old, Actor);
	}
}

void UMusubiEntityRegistry::Unregister(const FString& EntityId, AActor* Actor)
{
	TWeakObjectPtr<AActor>* P = Actors.Find(EntityId);
	if (P && (!P->IsValid() || P->Get() == Actor)) Actors.Remove(EntityId);
}

AActor* UMusubiEntityRegistry::RehomeKeptPlayerActor(const UClass* Cls) const
{
	if (!bRehoming || !Cls) return nullptr;
	for (AActor* A : { RehomeKeptPC.Get(), RehomeKeptPS.Get() })
		if (IsValid(A) && A->IsA(Cls)) return A;
	return nullptr;
}

void UMusubiEntityRegistry::SetRehoming(bool bOn)
{
	bRehoming = bOn;
	IrisKept = 0; IrisReused = 0;
	if (!bOn) { RehomeKeptPC.Reset(); RehomeKeptPS.Reset(); }
}

void UMusubiEntityRegistry::SetRehomeKeptPlayer(AActor* PC, AActor* PS)
{
	RehomeKeptPC = PC;
	RehomeKeptPS = PS;
}

void UMusubiEntityRegistry::NoteRehomeKeptPlayerReused(const AActor* A)
{
	if (A && A == RehomeKeptPC.Get()) RehomeKeptPC.Reset();
	if (A && A == RehomeKeptPS.Get()) RehomeKeptPS.Reset();
	++IrisReused;
}
