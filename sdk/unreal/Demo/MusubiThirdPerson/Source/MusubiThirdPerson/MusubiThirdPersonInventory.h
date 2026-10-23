// The demo's replicated inventory: a FastArray of items plus gold with a RepNotify. Lives on the character, so it rides
// the entity through a handover (binary iris state, docs §9.8) and reaches the player's client from whichever server owns it.
#pragma once

#include "CoreMinimal.h"
#include "Components/ActorComponent.h"
#include "Net/Serialization/FastArraySerializer.h"
#include "MusubiThirdPersonInventory.generated.h"

USTRUCT(BlueprintType)
struct FMusubiThirdPersonItem : public FFastArraySerializerItem
{
	GENERATED_BODY()
	UPROPERTY(BlueprintReadOnly, Category = "Inventory") int32 Id = 0;
	UPROPERTY(BlueprintReadOnly, Category = "Inventory") int32 Count = 0;
};

USTRUCT()
struct FMusubiThirdPersonItems : public FFastArraySerializer
{
	GENERATED_BODY()
	UPROPERTY() TArray<FMusubiThirdPersonItem> Items;
	bool NetDeltaSerialize(FNetDeltaSerializeInfo& P) { return FFastArraySerializer::FastArrayDeltaSerialize<FMusubiThirdPersonItem, FMusubiThirdPersonItems>(Items, P, *this); }
};
template<> struct TStructOpsTypeTraits<FMusubiThirdPersonItems> : public TStructOpsTypeTraitsBase2<FMusubiThirdPersonItems> { enum { WithNetDeltaSerializer = true }; };

DECLARE_DYNAMIC_MULTICAST_DELEGATE(FMusubiThirdPersonInventoryChanged);

UCLASS(ClassGroup = "Musubi", meta = (BlueprintSpawnableComponent))
class MUSUBITHIRDPERSON_API UMusubiThirdPersonInventory : public UActorComponent
{
	GENERATED_BODY()
public:
	UMusubiThirdPersonInventory() { SetIsReplicatedByDefault(true); }
	virtual void GetLifetimeReplicatedProps(TArray<FLifetimeProperty>& OutLifetimeProps) const override;

	UPROPERTY(Replicated) FMusubiThirdPersonItems Items;
	UPROPERTY(ReplicatedUsing = OnRep_Gold, BlueprintReadOnly, Category = "Inventory") int32 Gold = 0;

	/** Owner only (the server that owns the entity): add `Count` of item `Id`. */
	UFUNCTION(BlueprintCallable, Category = "Inventory") void AddItem(int32 Id, int32 Count);
	UFUNCTION(BlueprintCallable, Category = "Inventory") void AddGold(int32 Amount);
	UFUNCTION(BlueprintPure, Category = "Inventory") int32 NumItems() const { return Items.Items.Num(); }
	UFUNCTION(BlueprintPure, Category = "Inventory") TArray<FMusubiThirdPersonItem> GetItems() const { return Items.Items; }
	/** Fires on every side that sees a change (owner, ghost server, client). */
	UPROPERTY(BlueprintAssignable, Category = "Inventory") FMusubiThirdPersonInventoryChanged OnInventoryChanged;

	UFUNCTION() void OnRep_Gold() { OnInventoryChanged.Broadcast(); }
};
