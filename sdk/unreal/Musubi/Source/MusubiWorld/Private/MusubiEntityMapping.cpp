#include "MusubiEntityMapping.h"
#include "MusubiJson.h"

static bool JsonEqualsText(const FMusubiJson& V, const FString& Text)
{
	if (!V.Value.IsValid()) return Text.IsEmpty() || Text == TEXT("null");
	FString S;
	if (V.Value->TryGetString(S)) return S == Text;
	return V.ToText() == Text;
}

const FMusubiEntityRule* UMusubiEntityMapping::Resolve(const FMusubiWorldEntity& E) const
{
	for (const FMusubiEntityRule& R : Rules)
	{
		if (R.Component.IsEmpty()) return &R;
		const FMusubiWorldComponent* C = E.components.FindByPredicate([&](const FMusubiWorldComponent& X) { return X.name == R.Component; });
		if (!C) continue;
		if (!R.Equals.IsEmpty() && !JsonEqualsText(C->value, R.Equals)) continue;
		return &R;
	}
	return nullptr;
}
