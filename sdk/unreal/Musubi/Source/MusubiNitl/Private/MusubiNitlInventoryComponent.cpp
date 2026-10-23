#include "MusubiNitlInventoryComponent.h"
#include "Net/UnrealNetwork.h"
#include "GameFramework/Actor.h"

DEFINE_LOG_CATEGORY_STATIC(LogMusubiNitlInv, Log, All);

void UMusubiNitlInventoryComponent::GetLifetimeReplicatedProps(TArray<FLifetimeProperty>& OutLifetimeProps) const
{
	Super::GetLifetimeReplicatedProps(OutLifetimeProps);
	DOREPLIFETIME(UMusubiNitlInventoryComponent, Items);
	DOREPLIFETIME(UMusubiNitlInventoryComponent, Gold);
}

void UMusubiNitlInventoryComponent::OnRep_Gold()
{
	UE_LOG(LogMusubiNitlInv, Verbose, TEXT("%s gold=%d items=%d"), *GetOwner()->GetName(), Gold, NumItems());
}

void UMusubiNitlInventoryComponent::AddItem(int32 Id, int32 Count)
{
	FMusubiNitlItem& I = Items.Items.AddDefaulted_GetRef();
	I.Id = Id;
	I.Count = Count;
	Items.MarkItemDirty(I);
}
