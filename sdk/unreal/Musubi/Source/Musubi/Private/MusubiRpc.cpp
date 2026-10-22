#include "MusubiRpc.h"
#include "MusubiSubsystem.h"
#include "MusubiClient.h"

void UMusubiRpc::Call(const FString& Id, const FMusubiJson& Payload, TFunction<void(bool, const FMusubiJson&, const FMusubiServerEnvelope&)> OnDone, bool bFallbackToRest)
{
	EnsureBound();
	TSharedPtr<FMusubiSocket> S = Socket();
	if ((!S.IsValid() || !S->IsConnected()) && bFallbackToRest)
	{
		UMusubiSubsystem* M = Musubi();
		TSharedPtr<FMusubiClient> Http = M ? M->Client() : nullptr;
		if (!Http.IsValid())
		{
			if (OnDone) OnDone(false, FMusubiJson(), LocalError(EMusubiCodeTag::Unavailable, TEXT("no socket and no HTTP client")));
			return;
		}
		TSharedPtr<FJsonObject> Body = Payload.Value.IsValid() && Payload.Value->Type == EJson::Object ? Payload.Value->AsObject() : MakeShared<FJsonObject>();
		Http->Rpc(Id, Body, FMusubiJsonResult::CreateLambda([OnDone](bool bOk, TSharedPtr<FJsonObject> Reply) {
			FMusubiJson J;
			if (Reply.IsValid()) J.Value = MakeShared<FJsonValueObject>(Reply);
			if (OnDone) OnDone(bOk, J, bOk ? FMusubiServerEnvelope() : LocalError(EMusubiCodeTag::Unavailable, TEXT("rpc over REST failed")));
		}));
		return;
	}
	FMusubiClientMessageRpc R;
	R.id = Id;
	R.payload = Payload;
	FMusubiClientMessage Msg;
	Msg.Tag = EMusubiClientMessageTag::Rpc;
	Msg.Rpc = MoveTemp(R);
	Request(MoveTemp(Msg), Expect<FMusubiServerMessageRpc>(EMusubiServerMessageTag::Rpc, &FMusubiServerMessage::Rpc,
		[OnDone](const FMusubiServerMessageRpc& Reply) { if (OnDone) OnDone(true, Reply.payload, FMusubiServerEnvelope()); },
		[OnDone](const FMusubiServerEnvelope& E) { if (OnDone) OnDone(false, FMusubiJson(), E); }));
}

void UMusubiRpc::CallRpc(const FString& Id, const FString& PayloadJson, FMusubiRpcDone OnDone)
{
	Call(Id, PayloadJson.IsEmpty() ? FMusubiJson() : FMusubiJson::FromText(PayloadJson), [OnDone](bool bOk, const FMusubiJson& P, const FMusubiServerEnvelope& E) {
		OnDone.ExecuteIfBound(bOk, bOk ? P.ToText() : FString(), bOk ? FString() : (E.Msg.Error.IsSet() ? E.Msg.Error->message : TEXT("error")));
	});
}
