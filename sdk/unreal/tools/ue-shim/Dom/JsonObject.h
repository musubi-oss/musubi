#pragma once
#include "Dom/JsonValue.h"
class FJsonObject
{
public:
	TMap<FString, TSharedPtr<FJsonValue>> Values;
	// UE preserves insertion order; the golden tests compare parsed values, not text, so std::map is fine here.
	void SetField(const FString& K, const TSharedPtr<FJsonValue>& V) { Values.Add(K, V); }
	TSharedPtr<FJsonValue> TryGetField(const FString& K) const { const TSharedPtr<FJsonValue>* V = Values.Find(K); return V ? *V : nullptr; }
	bool TryGetStringField(const FString& K, FString& Out) const { TSharedPtr<FJsonValue> V = TryGetField(K); return V && V->TryGetString(Out); }
};
