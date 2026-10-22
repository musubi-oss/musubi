// Base for the per-domain realtime facades (presence, chat, parties, …): each one is a
// GameInstance subsystem that owns a slice of state mirrored from the node and exposes typed
// requests over UMusubiSubsystem's FMusubiSocket. See docs/gdk-unreal.md §3.4.
#pragma once

#include "CoreMinimal.h"
#include "Subsystems/GameInstanceSubsystem.h"
#include "MusubiSocket.h"
#include "MusubiFacade.generated.h"

class UMusubiSubsystem;

UCLASS(Abstract)
class MUSUBI_API UMusubiFacade : public UGameInstanceSubsystem
{
	GENERATED_BODY()
public:
	virtual void Initialize(FSubsystemCollectionBase& Collection) override;
	virtual void Deinitialize() override;

	UMusubiSubsystem* Musubi() const;
	TSharedPtr<FMusubiSocket> Socket() const;

protected:
	/** Bind per-tag listeners here; called once per socket instance (and again if the subsystem swaps sockets). */
	virtual void BindSocket(FMusubiSocket& S) {}
	/** The socket reopened after a drop: re-issue subscriptions/joins that the node forgot. */
	virtual void OnResubscribe() {}
	/** The socket closed (any reason): clear state that only lives while connected. */
	virtual void OnDisconnected() {}

	/** Fire-and-forget; false when there is no open socket. */
	bool Send(FMusubiClientMessage&& Msg);
	/** Request with reply correlation; without a socket the reply fires immediately with a synthetic `unavailable` error. */
	void Request(FMusubiClientMessage&& Msg, FMusubiReply OnReply);
	/** Subscribe to one server tag for the lifetime of this facade. */
	void Listen(EMusubiServerMessageTag Tag, TFunction<void(const FMusubiServerMessage&)> Fn);
	/** Build an error envelope for local failures (no socket, bad argument) so callers see one shape. */
	static FMusubiServerEnvelope LocalError(EMusubiCodeTag Code, const FString& Message);
	/** `OnReply` adapter: calls `Ok(Payload)` when the reply carries `Tag`, else `Err(ErrorEnvelope)`. */
	template <typename TPayload>
	static FMusubiReply Expect(EMusubiServerMessageTag Tag, const TOptional<TPayload> FMusubiServerMessage::*Field,
		TFunction<void(const TPayload&)> Ok, TFunction<void(const FMusubiServerEnvelope&)> Err)
	{
		return FMusubiReply::CreateLambda([Tag, Field, Ok, Err](bool bOk, const FMusubiServerEnvelope& E) {
			if (bOk && E.Msg.Tag == Tag && (E.Msg.*Field).IsSet()) { if (Ok) Ok((E.Msg.*Field).GetValue()); }
			else if (Err) Err(bOk ? LocalError(EMusubiCodeTag::Internal, FString::Printf(TEXT("unexpected reply tag %s"), MusubiTagName(E.Msg.Tag))) : E);
		});
	}

	/** Re-check the subsystem's socket and (re)bind if it changed. */
	void EnsureBound();

private:
	TWeakPtr<FMusubiSocket> Bound;
	TArray<TPair<EMusubiServerMessageTag, FDelegateHandle>> TagHandles;
	TArray<TPair<EMusubiServerMessageTag, TFunction<void(const FMusubiServerMessage&)>>> Listeners;
	FDelegateHandle ResubHandle;
	FDelegateHandle ClosedHandle;
	void Unbind();
};

/** Common Blueprint-facing result shape for facade calls. */
USTRUCT(BlueprintType)
struct MUSUBI_API FMusubiCallResult
{
	GENERATED_BODY()
	UPROPERTY(BlueprintReadOnly, Category = "Musubi") bool bOk = false;
	/** Wire error code tag (`not_found`, `permission_denied`, …) when !bOk. */
	UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString Code;
	UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString Message;
	/** Reply payload as JSON text (the typed C++ API gets the struct instead). */
	UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString PayloadJson;

	static FMusubiCallResult From(bool bOk, const FMusubiServerEnvelope& E);
};
DECLARE_DYNAMIC_DELEGATE_OneParam(FMusubiCallDone, const FMusubiCallResult&, Result);
