// NITL: a replicated component with a FastArray and a RepNotify int on the entity pawn. The owner grows it while it
// walks; the ghost on the other server must see it through the binary iris export ("sub") and the new owner must
// continue from the same values after the handover, with no reset.
#pragma once

#include "CoreMinimal.h"
#include "Components/ActorComponent.h"
#include "Net/Serialization/FastArraySerializer.h"
#include "MusubiNitlInventoryComponent.generated.h"

USTRUCT()
struct FMusubiNitlItem : public FFastArraySerializerItem
{
	GENERATED_BODY()
	UPROPERTY() int32 Id = 0;
	UPROPERTY() int32 Count = 0;
};

USTRUCT()
struct FMusubiNitlItems : public FFastArraySerializer
{
	GENERATED_BODY()
	UPROPERTY() TArray<FMusubiNitlItem> Items;
	bool NetDeltaSerialize(FNetDeltaSerializeInfo& P) { return FFastArraySerializer::FastArrayDeltaSerialize<FMusubiNitlItem, FMusubiNitlItems>(Items, P, *this); }
};
template<> struct TStructOpsTypeTraits<FMusubiNitlItems> : public TStructOpsTypeTraitsBase2<FMusubiNitlItems> { enum { WithNetDeltaSerializer = true }; };

UCLASS()
class MUSUBINITL_API UMusubiNitlInventoryComponent : public UActorComponent
{
	GENERATED_BODY()
public:
	UMusubiNitlInventoryComponent() { SetIsReplicatedByDefault(true); }
	virtual void GetLifetimeReplicatedProps(TArray<FLifetimeProperty>& OutLifetimeProps) const override;
	UPROPERTY(Replicated) FMusubiNitlItems Items;
	UPROPERTY(ReplicatedUsing = OnRep_Gold) int32 Gold = 0;
	UFUNCTION() void OnRep_Gold();
	void AddItem(int32 Id, int32 Count);
	int32 NumItems() const { return Items.Items.Num(); }
};
