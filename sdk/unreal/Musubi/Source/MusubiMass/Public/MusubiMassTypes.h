#pragma once

#include "CoreMinimal.h"
#include "MassEntityTypes.h"
#include "MusubiWireTypes.h"
#include "MusubiMassTypes.generated.h"

/** Identity of a Musubi world entity on its Mass entity. Plain data: fragments are moved by memcpy inside Mass. */
USTRUCT()
struct MUSUBIMASS_API FMusubiEntityFragment : public FMassFragment
{
	GENERATED_BODY()

	/** Entity uuid, 16 bytes as two big-endian halves (FMusubiUuid::Bytes[0..8], [8..16]). */
	uint64 IdHi = 0;
	uint64 IdLo = 0;
	/** Newest authoritative tick applied to the record. */
	uint64 LastTick = 0;
	/** Bumped whenever any component value changed (projects re-read components on change only). */
	uint32 ComponentsRevision = 0;
	/** True for the client's own avatar (UMusubiWorldComponent::SetLocalEntity). */
	bool bLocal = false;

	FMusubiUuid Uuid() const;
	void SetUuid(const FMusubiUuid& U);
};

/** Marks Mass entities owned by a UMusubiMassSinkComponent (query tag). */
USTRUCT()
struct MUSUBIMASS_API FMusubiEntityTag : public FMassTag
{
	GENERATED_BODY()
};
