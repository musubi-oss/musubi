#include "MusubiFacade.h"
#include "MusubiSubsystem.h"
#include "MusubiEnvelope.h"

void UMusubiFacade::Initialize(FSubsystemCollectionBase& Collection)
{
	Super::Initialize(Collection);
	Collection.InitializeDependency<UMusubiSubsystem>();
	EnsureBound();
}

void UMusubiFacade::Deinitialize()
{
	Unbind();
	Listeners.Empty();
	Super::Deinitialize();
}

UMusubiSubsystem* UMusubiFacade::Musubi() const
{
	UGameInstance* GI = GetGameInstance();
	return GI ? GI->GetSubsystem<UMusubiSubsystem>() : nullptr;
}

TSharedPtr<FMusubiSocket> UMusubiFacade::Socket() const
{
	UMusubiSubsystem* M = Musubi();
	return M ? M->Socket() : nullptr;
}

void UMusubiFacade::EnsureBound()
{
	TSharedPtr<FMusubiSocket> S = Socket();
	if (!S.IsValid() || Bound.Pin() == S) return;
	Unbind();
	Bound = S;
	TWeakObjectPtr<UMusubiFacade> Weak(this);
	for (const auto& L : Listeners)
	{
		TFunction<void(const FMusubiServerMessage&)> Fn = L.Value;
		FDelegateHandle H = S->OnTag(L.Key).AddLambda([Weak, Fn](const FMusubiServerEnvelope& E) { if (Weak.IsValid()) Fn(E.Msg); });
		TagHandles.Emplace(L.Key, H);
	}
	ResubHandle = S->OnResubscribe.AddLambda([Weak]() { if (Weak.IsValid()) Weak->OnResubscribe(); });
	ClosedHandle = S->OnClosed.AddLambda([Weak](int32, const FString&) { if (Weak.IsValid()) Weak->OnDisconnected(); });
	BindSocket(*S);
}

void UMusubiFacade::Unbind()
{
	if (TSharedPtr<FMusubiSocket> S = Bound.Pin())
	{
		for (const auto& H : TagHandles) S->OnTag(H.Key).Remove(H.Value);
		S->OnResubscribe.Remove(ResubHandle);
		S->OnClosed.Remove(ClosedHandle);
	}
	TagHandles.Empty();
	Bound.Reset();
}

void UMusubiFacade::Listen(EMusubiServerMessageTag Tag, TFunction<void(const FMusubiServerMessage&)> Fn)
{
	Listeners.Emplace(Tag, Fn);
	if (TSharedPtr<FMusubiSocket> S = Bound.Pin())
	{
		TWeakObjectPtr<UMusubiFacade> Weak(this);
		TFunction<void(const FMusubiServerMessage&)> Copy = Fn;
		TagHandles.Emplace(Tag, S->OnTag(Tag).AddLambda([Weak, Copy](const FMusubiServerEnvelope& E) { if (Weak.IsValid()) Copy(E.Msg); }));
	}
}

bool UMusubiFacade::Send(FMusubiClientMessage&& Msg)
{
	EnsureBound();
	TSharedPtr<FMusubiSocket> S = Socket();
	return S.IsValid() && S->Send(Msg);
}

void UMusubiFacade::Request(FMusubiClientMessage&& Msg, FMusubiReply OnReply)
{
	EnsureBound();
	TSharedPtr<FMusubiSocket> S = Socket();
	if (!S.IsValid() || S->GetState() != EMusubiSocketState::Open)
	{
		OnReply.ExecuteIfBound(false, LocalError(EMusubiCodeTag::Unavailable, TEXT("realtime socket is not connected")));
		return;
	}
	S->Request(Msg, MoveTemp(OnReply));
}

FMusubiServerEnvelope UMusubiFacade::LocalError(EMusubiCodeTag Code, const FString& Message)
{
	FMusubiServerEnvelope E;
	E.Msg.Tag = EMusubiServerMessageTag::Error;
	FMusubiServerMessageError Err;
	Err.code.Tag = Code;
	Err.message = Message;
	E.Msg.Error = MoveTemp(Err);
	return E;
}

FMusubiCallResult FMusubiCallResult::From(bool bOk, const FMusubiServerEnvelope& E)
{
	FMusubiCallResult R;
	R.bOk = bOk;
	if (!bOk && E.Msg.Error.IsSet())
	{
		R.Code = MusubiTagName(E.Msg.Error->code.Tag);
		R.Message = E.Msg.Error->message;
	}
	else
	{
		R.PayloadJson = MusubiJson::Serialize(FMusubiServerMessage::ToJson(E.Msg));
	}
	return R;
}
