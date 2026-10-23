#include "MusubiNitlLoadout.h"
#include "Net/UnrealNetwork.h"

void UMusubiNitlLoadout::GetLifetimeReplicatedProps(TArray<FLifetimeProperty>& OutLifetimeProps) const
{
	Super::GetLifetimeReplicatedProps(OutLifetimeProps);
	DOREPLIFETIME(UMusubiNitlLoadout, Ammo);
	DOREPLIFETIME(UMusubiNitlLoadout, Weapon);
}
