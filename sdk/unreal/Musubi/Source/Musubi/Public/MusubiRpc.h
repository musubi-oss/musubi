// RPC facade: server functions by id over the realtime socket (wire tag rpc → rpc); falls back to
// REST (`POST /v1/rpc/{id}`) through FMusubiClient when the socket is down.
#pragma once

#include "MusubiFacade.h"
#include "MusubiRpc.generated.h"

DECLARE_DYNAMIC_DELEGATE_ThreeParams(FMusubiRpcDone, bool, bOk, const FString&, PayloadJson, const FString&, Error);

UCLASS()
class MUSUBI_API UMusubiRpc : public UMusubiFacade
{
	GENERATED_BODY()
public:
	/** Call over the socket; `OnDone(ok, payload, errorEnvelope)`. If `bFallbackToRest` and the socket is not open, uses REST instead. */
	void Call(const FString& Id, const FMusubiJson& Payload, TFunction<void(bool, const FMusubiJson&, const FMusubiServerEnvelope&)> OnDone, bool bFallbackToRest = true);

	UFUNCTION(BlueprintCallable, Category = "Musubi|Rpc")
	void CallRpc(const FString& Id, const FString& PayloadJson, FMusubiRpcDone OnDone);
};
