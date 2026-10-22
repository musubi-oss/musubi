#include "MusubiEnvelope.h"
#include "MusubiJson.h"
#include "MusubiPostcard.h"

namespace
{
	template <typename TMsg>
	FString EnvelopeToJson(const TOptional<FString>& Cid, const TMsg& Msg)
	{
		TSharedPtr<FJsonObject> O = MakeShared<FJsonObject>();
		O->SetField(TEXT("cid"), Cid.IsSet() ? TSharedPtr<FJsonValue>(MakeShared<FJsonValueString>(Cid.GetValue())) : MusubiJson::Null());
		O->SetField(TEXT("msg"), TMsg::ToJson(Msg));
		return MusubiJson::Serialize(MakeShared<FJsonValueObject>(O));
	}

	template <typename TMsg>
	bool EnvelopeFromJson(const FString& Text, TOptional<FString>& Cid, TMsg& Msg)
	{
		TSharedPtr<FJsonValue> J = MusubiJson::Deserialize(Text);
		const TSharedPtr<FJsonObject>* O;
		if (!J.IsValid() || !J->TryGetObject(O)) return false;
		FString C;
		if ((*O)->TryGetStringField(TEXT("cid"), C)) Cid = MoveTemp(C); else Cid.Reset();
		TSharedPtr<FJsonValue> M = (*O)->TryGetField(TEXT("msg"));
		return M.IsValid() && TMsg::FromJson(M, Msg);
	}

	template <typename TMsg>
	TArray<uint8> EnvelopeToPostcard(const TOptional<FString>& Cid, const TMsg& Msg)
	{
		FMusubiPostcardWriter W;
		if (Cid.IsSet()) { W.Byte(1); W.Str(Cid.GetValue()); } else W.Byte(0);
		TMsg::Write(W, Msg);
		return W.Ok() ? W.Take() : TArray<uint8>();
	}

	template <typename TMsg>
	bool EnvelopeFromPostcard(const uint8* Data, int32 Len, TOptional<FString>& Cid, TMsg& Msg)
	{
		FMusubiPostcardReader R(Data, Len);
		if (R.Byte() == 1) Cid = R.Str(); else Cid.Reset();
		TMsg::Read(R, Msg);
		return R.Finished();
	}
}

FString FMusubiClientEnvelope::ToJsonText() const { return EnvelopeToJson(Cid, Msg); }
TArray<uint8> FMusubiClientEnvelope::ToPostcard() const { return EnvelopeToPostcard(Cid, Msg); }
bool FMusubiClientEnvelope::FromJsonText(const FString& Text) { return EnvelopeFromJson(Text, Cid, Msg); }
bool FMusubiClientEnvelope::FromPostcard(const uint8* Data, int32 Len) { return EnvelopeFromPostcard(Data, Len, Cid, Msg); }

FString FMusubiServerEnvelope::ToJsonText() const { return EnvelopeToJson(Cid, Msg); }
TArray<uint8> FMusubiServerEnvelope::ToPostcard() const { return EnvelopeToPostcard(Cid, Msg); }
bool FMusubiServerEnvelope::FromJsonText(const FString& Text) { return EnvelopeFromJson(Text, Cid, Msg); }
bool FMusubiServerEnvelope::FromPostcard(const uint8* Data, int32 Len) { return EnvelopeFromPostcard(Data, Len, Cid, Msg); }
