#include "MusubiEntityComponent.h"

FString UMusubiEntityComponent::GetComponentJson(const FString& Name) const
{
	const FMusubiEntityComponentState* S = Components.Find(Name);
	return S ? S->Value.ToText() : FString();
}

FString UMusubiEntityComponent::GetComponentString(const FString& Name, const FString& Default) const
{
	const FMusubiEntityComponentState* S = Components.Find(Name);
	FString Out;
	if (S && S->Value.Value.IsValid() && S->Value.Value->TryGetString(Out)) return Out;
	return Default;
}

double UMusubiEntityComponent::GetComponentNumber(const FString& Name, double Default) const
{
	const FMusubiEntityComponentState* S = Components.Find(Name);
	double Out = 0;
	if (S && S->Value.Value.IsValid() && S->Value.Value->TryGetNumber(Out)) return Out;
	return Default;
}

TArray<FString> UMusubiEntityComponent::ComponentNames() const
{
	TArray<FString> Out;
	Components.GetKeys(Out);
	return Out;
}

const FMusubiJson* UMusubiEntityComponent::FindComponent(const FString& Name) const
{
	const FMusubiEntityComponentState* S = Components.Find(Name);
	return S ? &S->Value : nullptr;
}

void UMusubiEntityComponent::Reset(const FMusubiUuid& Id, bool bIsLocal)
{
	EntityId = Id;
	bLocal = bIsLocal;
	LastTick = 0;
	Components.Reset();
}

void UMusubiEntityComponent::SetComponent(const FString& Name, const FMusubiJson& Value, uint64 Version)
{
	FMusubiEntityComponentState& S = Components.FindOrAdd(Name);
	S.Value = Value;
	S.Version = Version;
	OnComponentChanged.Broadcast(Name, Value.ToText());
}

AMusubiEntityActor::AMusubiEntityActor()
{
	PrimaryActorTick.bCanEverTick = false;
	Entity = CreateDefaultSubobject<UMusubiEntityComponent>(TEXT("Entity"));
}
