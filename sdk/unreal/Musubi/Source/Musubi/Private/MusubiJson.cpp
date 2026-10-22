#include "MusubiJson.h"
#include "Serialization/JsonSerializer.h"
#include "Serialization/JsonWriter.h"
#include "Serialization/JsonReader.h"

TSharedPtr<FJsonObject> FMusubiJson::AsObject() const
{
	const TSharedPtr<FJsonObject>* O;
	return (Value.IsValid() && Value->TryGetObject(O)) ? *O : nullptr;
}

FString FMusubiJson::ToText() const
{
	return Value.IsValid() ? MusubiJson::Serialize(Value) : TEXT("null");
}

FMusubiJson FMusubiJson::FromText(const FString& Text)
{
	return FMusubiJson(MusubiJson::Deserialize(Text));
}

namespace MusubiJson
{
	TSharedPtr<FJsonValue> Null()
	{
		return MakeShared<FJsonValueNull>();
	}

	TSharedPtr<FJsonValue> JsonOrNull(const FMusubiJson& V)
	{
		return V.Value.IsValid() ? V.Value : Null();
	}

	TSharedPtr<FJsonValue> Tagged(const TCHAR* Tag, const TSharedPtr<FJsonValue>& Payload)
	{
		TSharedPtr<FJsonObject> O = MakeShared<FJsonObject>();
		O->SetField(Tag, Payload.IsValid() ? Payload : Null());
		return MakeShared<FJsonValueObject>(O);
	}

	bool Untag(const TSharedPtr<FJsonValue>& J, FString& Tag, TSharedPtr<FJsonValue>& Payload)
	{
		if (!J.IsValid()) return false;
		if (J->TryGetString(Tag))
		{
			Payload.Reset();
			return true;
		}
		const TSharedPtr<FJsonObject>* O;
		if (J->TryGetObject(O) && (*O)->Values.Num() == 1)
		{
			const auto& P = *(*O)->Values.CreateConstIterator();
			Tag = P.Key;
			Payload = P.Value;
			return true;
		}
		return false;
	}

	TSharedPtr<FJsonValue> BytesToJson(const TArray<uint8>& B)
	{
		TArray<TSharedPtr<FJsonValue>> Arr;
		Arr.Reserve(B.Num());
		for (uint8 X : B) Arr.Add(MakeShared<FJsonValueNumber>((double)X));
		return MakeShared<FJsonValueArray>(Arr);
	}

	bool BytesFromJson(const TSharedPtr<FJsonValue>& J, TArray<uint8>& Out)
	{
		const TArray<TSharedPtr<FJsonValue>>* Arr;
		if (!J.IsValid() || !J->TryGetArray(Arr)) return false;
		Out.Reset(Arr->Num());
		for (const TSharedPtr<FJsonValue>& V : *Arr)
		{
			double D;
			if (!V->TryGetNumber(D) || D < 0 || D > 255) return false;
			Out.Add((uint8)D);
		}
		return true;
	}

	FString Serialize(const TSharedPtr<FJsonValue>& V)
	{
		FString Out;
		TSharedRef<TJsonWriter<TCHAR, TCondensedJsonPrintPolicy<TCHAR>>> W = TJsonWriterFactory<TCHAR, TCondensedJsonPrintPolicy<TCHAR>>::Create(&Out);
		const TSharedPtr<FJsonObject>* O;
		const TArray<TSharedPtr<FJsonValue>>* A;
		if (V->TryGetObject(O))
		{
			FJsonSerializer::Serialize((*O).ToSharedRef(), W);
		}
		else if (V->TryGetArray(A))
		{
			FJsonSerializer::Serialize(*A, W);
		}
		else
		{
			// Scalars: FJsonSerializer only writes containers; wrap and strip.
			TArray<TSharedPtr<FJsonValue>> Wrap{ V };
			FJsonSerializer::Serialize(Wrap, W);
			W->Close();
			return Out.Mid(1, Out.Len() - 2);
		}
		W->Close();
		return Out;
	}

	TSharedPtr<FJsonValue> Deserialize(const FString& Text)
	{
		TSharedRef<TJsonReader<TCHAR>> R = TJsonReaderFactory<TCHAR>::Create(Text);
		TSharedPtr<FJsonValue> V;
		if (FJsonSerializer::Deserialize(R, V) && V.IsValid())
		{
			return V;
		}
		// FJsonSerializer::Deserialize(reader, value) rejects bare scalars on some versions; wrap.
		TSharedRef<TJsonReader<TCHAR>> R2 = TJsonReaderFactory<TCHAR>::Create(TEXT("[") + Text + TEXT("]"));
		TArray<TSharedPtr<FJsonValue>> Arr;
		if (FJsonSerializer::Deserialize(R2, Arr) && Arr.Num() == 1)
		{
			return Arr[0];
		}
		return nullptr;
	}
}
