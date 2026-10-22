// Chat facade: joined channels with presence + bounded history, typed send/update/remove,
// typing and reactions; channels are re-joined after a reconnect (wire tags channel_*).
#pragma once

#include "MusubiFacade.h"
#include "MusubiChat.generated.h"

struct MUSUBI_API FMusubiChannelState
{
	FString Id;
	EMusubiChannelKind Kind = EMusubiChannelKind::Room;
	/** What we joined with — used to re-join after a reconnect (the id may change for direct/group). */
	FMusubiClientMessageChannelJoin JoinArgs;
	FMusubiUserPresence Me;
	TArray<FMusubiUserPresence> Presences;
	/** Newest last; capped at `UMusubiChat::HistoryCap`. */
	TArray<FMusubiChannelMessage> History;
	const FMusubiChannelMessage* FindMessage(const FString& MessageId) const;
};

DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiChannelEvent, const FMusubiChannelState&);
DECLARE_MULTICAST_DELEGATE_TwoParams(FMusubiChannelMessageEvent, const FMusubiChannelState&, const FMusubiChannelMessage&);
DECLARE_MULTICAST_DELEGATE_TwoParams(FMusubiChannelPresenceDelegate, const FMusubiChannelState&, const FMusubiServerMessageChannelPresenceEvent&);
DECLARE_MULTICAST_DELEGATE_TwoParams(FMusubiChannelTypingDelegate, const FMusubiChannelState&, const FMusubiUserPresence&);
DECLARE_MULTICAST_DELEGATE_TwoParams(FMusubiChannelReactionDelegate, const FMusubiChannelState&, const FMusubiServerMessageChannelMessageReaction&);

DECLARE_DYNAMIC_MULTICAST_DELEGATE_FourParams(FMusubiOnChatMessage, const FString&, ChannelId, const FString&, MessageId, const FString&, Username, const FString&, ContentJson);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FMusubiOnChatChannel, const FString&, ChannelId, bool, bJoined);

UCLASS()
class MUSUBI_API UMusubiChat : public UMusubiFacade
{
	GENERATED_BODY()
public:
	static constexpr int32 HistoryCap = 200;

	virtual void Initialize(FSubsystemCollectionBase& Collection) override;

	// ---- C++ API ----
	/** Join a room by name, a direct channel by account id, or a group by group id. */
	void Join(const FString& Target, EMusubiChannelKind Kind, bool bPersistence, bool bHidden,
		TFunction<void(bool, const FMusubiChannelState*, const FMusubiServerEnvelope&)> OnDone = nullptr);
	void Leave(const FString& ChannelId);
	/** Sends content; the ack (message id, timestamps) arrives through OnDone and the message is appended to history. */
	void SendMessage(const FString& ChannelId, const FMusubiJson& Content, TOptional<FString> ReplyTo = {},
		TFunction<void(bool, const FMusubiServerMessageChannelMessageAck*, const FMusubiServerEnvelope&)> OnDone = nullptr);
	void UpdateMessage(const FString& ChannelId, const FString& MessageId, const FMusubiJson& Content, FMusubiReply OnDone = FMusubiReply());
	void RemoveMessage(const FString& ChannelId, const FString& MessageId, FMusubiReply OnDone = FMusubiReply());
	/** Throttled on the caller's side: at most one per `TypingIntervalSec` per channel. */
	void Typing(const FString& ChannelId);

	const FMusubiChannelState* Channel(const FString& ChannelId) const { return Channels.Find(ChannelId); }
	const TMap<FString, FMusubiChannelState>& JoinedChannels() const { return Channels; }

	FMusubiChannelEvent OnJoined;
	FMusubiChannelEvent OnLeft;
	FMusubiChannelMessageEvent OnMessage;
	FMusubiChannelMessageEvent OnMessageUpdated;
	FMusubiChannelMessageEvent OnMessageRemoved;
	FMusubiChannelPresenceDelegate OnPresence;
	FMusubiChannelTypingDelegate OnTyping;
	FMusubiChannelReactionDelegate OnReaction;

	// ---- Blueprint ----
	UFUNCTION(BlueprintCallable, Category = "Musubi|Chat")
	void JoinRoom(const FString& Room, bool bPersistence, bool bHidden, FMusubiCallDone OnDone);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Chat")
	void JoinDirect(const FString& AccountId, bool bPersistence, FMusubiCallDone OnDone);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Chat")
	void JoinGroup(const FString& GroupId, bool bPersistence, bool bHidden, FMusubiCallDone OnDone);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Chat")
	void LeaveChannel(const FString& ChannelId);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Chat")
	void SendText(const FString& ChannelId, const FString& Text, FMusubiCallDone OnDone);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Chat")
	void SendJson(const FString& ChannelId, const FString& ContentJson, const FString& ReplyToMessageId, FMusubiCallDone OnDone);
	UFUNCTION(BlueprintCallable, Category = "Musubi|Chat")
	void SendTyping(const FString& ChannelId);
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Chat") FMusubiOnChatMessage OnChatMessage;
	UPROPERTY(BlueprintAssignable, Category = "Musubi|Chat") FMusubiOnChatChannel OnChatChannel;

	double TypingIntervalSec = 3.0;

protected:
	virtual void OnResubscribe() override;
	virtual void OnDisconnected() override;

private:
	TMap<FString, FMusubiChannelState> Channels;
	TMap<FString, double> LastTyping;
	FMusubiChannelState& Upsert(const FMusubiServerMessageChannel& C, const FMusubiClientMessageChannelJoin& Args);
	void Push(FMusubiChannelState& Ch, const FMusubiChannelMessage& M);
	void HandleMessage(const FMusubiChannelMessage& M);
};
