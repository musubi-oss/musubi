#include "MusubiAsyncActions.h"
#include "MusubiChat.h"
#include "MusubiStorage.h"
#include "MusubiNotifications.h"
#include "MusubiPresence.h"
#include "MusubiMatchmaker.h"
#include "MusubiParties.h"
#include "MusubiMatches.h"
#include "MusubiRpc.h"
#include "Engine/GameInstance.h"
#include "Engine/World.h"
#include "Engine/Engine.h"

static UGameInstance* GameInstanceOf(UObject* WorldContextObject)
{
	UWorld* W = GEngine ? GEngine->GetWorldFromContextObject(WorldContextObject, EGetWorldErrorMode::LogAndReturnNull) : nullptr;
	return W ? W->GetGameInstance() : nullptr;
}

template <class TFacade>
UMusubiAsyncAction* UMusubiAsyncAction::Make(UObject* WorldContextObject, TFunction<void(TFacade*, FMusubiCallDone)> Call)
{
	UGameInstance* GI = GameInstanceOf(WorldContextObject);
	TFacade* F = GI ? GI->GetSubsystem<TFacade>() : nullptr;
	if (!F) return Fail(WorldContextObject, TEXT("unavailable"), FString::Printf(TEXT("%s: no game instance / facade"), *TFacade::StaticClass()->GetName()));
	UMusubiAsyncAction* A = NewObject<UMusubiAsyncAction>(GI);
	TWeakObjectPtr<TFacade> Weak(F);
	A->Start = [Weak, Call](UMusubiAsyncAction* Self)
	{
		if (TFacade* P = Weak.Get()) Call(P, Self->Done());
		else
		{
			FMusubiCallResult R;
			R.Code = TEXT("unavailable");
			R.Message = TEXT("facade gone");
			Self->Finish(R);
		}
	};
	A->RegisterWithGameInstance(GI);
	return A;
}

UMusubiAsyncAction* UMusubiAsyncAction::Fail(UObject* WorldContextObject, const FString& Code, const FString& Message)
{
	UMusubiAsyncAction* A = NewObject<UMusubiAsyncAction>();
	A->Start = [Code, Message](UMusubiAsyncAction* Self)
	{
		FMusubiCallResult R;
		R.Code = Code;
		R.Message = Message;
		Self->Finish(R);
	};
	return A;
}

FMusubiCallDone UMusubiAsyncAction::Done()
{
	FMusubiCallDone D;
	D.BindDynamic(this, &UMusubiAsyncAction::HandleDone);
	return D;
}

void UMusubiAsyncAction::Activate()
{
	if (Start) Start(this);
}

void UMusubiAsyncAction::HandleDone(const FMusubiCallResult& Result)
{
	Finish(Result);
}

void UMusubiAsyncAction::HandleRpc(bool bOk, const FString& PayloadJson, const FString& Error)
{
	FMusubiCallResult R;
	R.bOk = bOk;
	R.PayloadJson = PayloadJson;
	if (!bOk)
	{
		R.Code = Error.IsEmpty() ? TEXT("internal") : Error;
		R.Message = Error;
	}
	Finish(R);
}

void UMusubiAsyncAction::Finish(const FMusubiCallResult& Result)
{
	if (bFinished) return;
	bFinished = true;
	(Result.bOk ? OnDone : OnFailed).Broadcast(Result);
	SetReadyToDestroy();
}

#define MUSUBI_ASYNC(Facade, Expr) \
	return Make<Facade>(WorldContextObject, [=](Facade* F, FMusubiCallDone Done) { F->Expr; })

UMusubiAsyncAction* UMusubiAsyncAction::ChatJoinRoom(UObject* WorldContextObject, const FString& Room, bool bPersistence, bool bHidden)
{
	MUSUBI_ASYNC(UMusubiChat, JoinRoom(Room, bPersistence, bHidden, Done));
}
UMusubiAsyncAction* UMusubiAsyncAction::ChatJoinDirect(UObject* WorldContextObject, const FString& AccountId, bool bPersistence)
{
	MUSUBI_ASYNC(UMusubiChat, JoinDirect(AccountId, bPersistence, Done));
}
UMusubiAsyncAction* UMusubiAsyncAction::ChatJoinGroup(UObject* WorldContextObject, const FString& GroupId, bool bPersistence, bool bHidden)
{
	MUSUBI_ASYNC(UMusubiChat, JoinGroup(GroupId, bPersistence, bHidden, Done));
}
UMusubiAsyncAction* UMusubiAsyncAction::ChatSendText(UObject* WorldContextObject, const FString& ChannelId, const FString& Text)
{
	MUSUBI_ASYNC(UMusubiChat, SendText(ChannelId, Text, Done));
}
UMusubiAsyncAction* UMusubiAsyncAction::ChatSendJson(UObject* WorldContextObject, const FString& ChannelId, const FString& ContentJson, const FString& ReplyToMessageId)
{
	MUSUBI_ASYNC(UMusubiChat, SendJson(ChannelId, ContentJson, ReplyToMessageId, Done));
}
UMusubiAsyncAction* UMusubiAsyncAction::StorageSubscribe(UObject* WorldContextObject, const FString& Collection, const FString& Key, const FString& OwnerId)
{
	MUSUBI_ASYNC(UMusubiStorage, SubscribeCollection(Collection, Key, OwnerId, Done));
}
UMusubiAsyncAction* UMusubiAsyncAction::NotificationsRefreshInbox(UObject* WorldContextObject)
{
	MUSUBI_ASYNC(UMusubiNotifications, RefreshInbox(Done));
}
UMusubiAsyncAction* UMusubiAsyncAction::PresenceFollow(UObject* WorldContextObject, const TArray<FString>& AccountIds)
{
	MUSUBI_ASYNC(UMusubiPresence, FollowAccounts(AccountIds, Done));
}
UMusubiAsyncAction* UMusubiAsyncAction::MatchmakerEnqueue(UObject* WorldContextObject, int32 MinCount, int32 MaxCount, const FString& Query, const TMap<FString, FString>& StringProperties, const TMap<FString, double>& NumericProperties, int32 CountMultiple)
{
	MUSUBI_ASYNC(UMusubiMatchmaker, Enqueue(MinCount, MaxCount, Query, StringProperties, NumericProperties, CountMultiple, Done));
}
UMusubiAsyncAction* UMusubiAsyncAction::PartyCreate(UObject* WorldContextObject, bool bOpen, int32 MaxSize, const FString& Label, bool bHidden)
{
	MUSUBI_ASYNC(UMusubiParties, CreateParty(bOpen, MaxSize, Label, bHidden, Done));
}
UMusubiAsyncAction* UMusubiAsyncAction::PartyJoin(UObject* WorldContextObject, const FString& PartyId)
{
	MUSUBI_ASYNC(UMusubiParties, JoinParty(PartyId, Done));
}
UMusubiAsyncAction* UMusubiAsyncAction::MatchCreate(UObject* WorldContextObject, const FString& Name, bool bSuppressPresence)
{
	MUSUBI_ASYNC(UMusubiMatches, CreateMatch(Name, bSuppressPresence, Done));
}
UMusubiAsyncAction* UMusubiAsyncAction::MatchJoin(UObject* WorldContextObject, const FString& MatchId, bool bSpectator)
{
	MUSUBI_ASYNC(UMusubiMatches, JoinMatch(MatchId, bSpectator, Done));
}
UMusubiAsyncAction* UMusubiAsyncAction::MatchJoinByToken(UObject* WorldContextObject, const FString& Token)
{
	MUSUBI_ASYNC(UMusubiMatches, JoinMatchByToken(Token, Done));
}
#undef MUSUBI_ASYNC

UMusubiAsyncAction* UMusubiAsyncAction::RpcCall(UObject* WorldContextObject, const FString& Id, const FString& PayloadJson)
{
	// FMusubiRpcDone has its own shape (bOk, payload, error); adapted in HandleRpc instead of going through Make<>.
	UGameInstance* GI = GameInstanceOf(WorldContextObject);
	UMusubiRpc* F = GI ? GI->GetSubsystem<UMusubiRpc>() : nullptr;
	if (!F) return Fail(WorldContextObject, TEXT("unavailable"), TEXT("UMusubiRpc: no game instance / facade"));
	UMusubiAsyncAction* A = NewObject<UMusubiAsyncAction>(GI);
	TWeakObjectPtr<UMusubiRpc> Weak(F);
	A->Start = [Weak, Id, PayloadJson](UMusubiAsyncAction* Self)
	{
		UMusubiRpc* P = Weak.Get();
		if (!P)
		{
			FMusubiCallResult R;
			R.Code = TEXT("unavailable");
			R.Message = TEXT("facade gone");
			Self->Finish(R);
			return;
		}
		FMusubiRpcDone D;
		D.BindDynamic(Self, &UMusubiAsyncAction::HandleRpc);
		P->CallRpc(Id, PayloadJson, D);
	};
	A->RegisterWithGameInstance(GI);
	return A;
}
