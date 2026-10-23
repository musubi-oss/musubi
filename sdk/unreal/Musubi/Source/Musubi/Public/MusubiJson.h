// JSON helpers for the generated wire structs: a value wrapper for free-form JSON fields
// (`serde_json::Value` on the Rust side) and the externally-tagged enum envelope helpers.
#pragma once

#include "CoreMinimal.h"
#include "Dom/JsonValue.h"
#include "Dom/JsonObject.h"

/** Free-form JSON. On the binary wire it travels as its JSON text (postcard cannot carry `Value`). */
struct MUSUBI_API FMusubiJson
{
	TSharedPtr<FJsonValue> Value;

	FMusubiJson() = default;
	explicit FMusubiJson(const TSharedPtr<FJsonValue>& In) : Value(In) {}
	explicit FMusubiJson(const TSharedPtr<FJsonObject>& In) : Value(In.IsValid() ? TSharedPtr<FJsonValue>(MakeShared<FJsonValueObject>(In)) : TSharedPtr<FJsonValue>()) {}

	bool IsValid() const { return Value.IsValid() && !Value->IsNull(); }
	TSharedPtr<FJsonObject> AsObject() const;
	FString ToText() const;
	static FMusubiJson FromText(const FString& Text);
	static FMusubiJson Object() { return FMusubiJson(MakeShared<FJsonObject>()); }
};

namespace MusubiJson
{
	MUSUBI_API TSharedPtr<FJsonValue> Null();
	MUSUBI_API TSharedPtr<FJsonValue> JsonOrNull(const FMusubiJson& V);
	/** `{"tag": payload}` */
	MUSUBI_API TSharedPtr<FJsonValue> Tagged(const TCHAR* Tag, const TSharedPtr<FJsonValue>& Payload);
	/** Accepts `"tag"` (unit variant) or `{"tag": payload}`; Payload is null for unit variants. */
	MUSUBI_API bool Untag(const TSharedPtr<FJsonValue>& J, FString& Tag, TSharedPtr<FJsonValue>& Payload);
	/** `Vec<u8>` is a JSON array of numbers in serde_json. */
	MUSUBI_API TSharedPtr<FJsonValue> BytesToJson(const TArray<uint8>& B);
	MUSUBI_API bool BytesFromJson(const TSharedPtr<FJsonValue>& J, TArray<uint8>& Out);

	template <typename T, typename F>
	TSharedPtr<FJsonValue> ArrayToJson(const TArray<T>& In, F Conv)
	{
		TArray<TSharedPtr<FJsonValue>> Arr;
		Arr.Reserve(In.Num());
		for (const T& V : In) Arr.Add(Conv(V));
		return MakeShared<FJsonValueArray>(Arr);
	}

	template <typename T, typename F>
	TSharedPtr<FJsonValue> MapToJson(const TMap<FString, T>& In, F Conv)
	{
		TSharedPtr<FJsonObject> O = MakeShared<FJsonObject>();
		for (const auto& P : In) O->SetField(P.Key, Conv(P.Value));
		return MakeShared<FJsonValueObject>(O);
	}

	MUSUBI_API FString Serialize(const TSharedPtr<FJsonValue>& V);
	MUSUBI_API TSharedPtr<FJsonValue> Deserialize(const FString& Text);
}
