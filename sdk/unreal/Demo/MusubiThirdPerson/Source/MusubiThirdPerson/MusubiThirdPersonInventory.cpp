#include "MusubiThirdPersonInventory.h"
#include "Net/UnrealNetwork.h"

void UMusubiThirdPersonInventory::GetLifetimeReplicatedProps(TArray<FLifetimeProperty>& OutLifetimeProps) const
{
	Super::GetLifetimeReplicatedProps(OutLifetimeProps);
	DOREPLIFETIME(UMusubiThirdPersonInventory, Items);
	DOREPLIFETIME(UMusubiThirdPersonInventory, Gold);
}

void UMusubiThirdPersonInventory::AddItem(int32 Id, int32 Count)
{
	for (FMusubiThirdPersonItem& I : Items.Items)
		if (I.Id == Id) { I.Count += Count; Items.MarkItemDirty(I); OnInventoryChanged.Broadcast(); return; }
	FMusubiThirdPersonItem& I = Items.Items.AddDefaulted_GetRef();
	I.Id = Id; I.Count = Count;
	Items.MarkItemDirty(I);
	OnInventoryChanged.Broadcast();
}

void UMusubiThirdPersonInventory::AddGold(int32 Amount)
{
	Gold += Amount;
	OnInventoryChanged.Broadcast();
}
