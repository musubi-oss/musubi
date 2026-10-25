#include "MusubiSubObjectFactory.h"
#include "MusubiIrisRehome.h"
#include "UObject/UObjectIterator.h"
#include "UObject/UObjectHash.h"
#include "Iris/ReplicationSystem/ObjectReplicationBridge.h"

#if ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION >= 7
using namespace UE::Net;

UNetObjectFactory::FInstantiateResult UMusubiSubObjectFactory::InstantiateReplicatedObjectFromHeader(const FInstantiateContext& Context, const FNetObjectCreationHeader* Header)
{
	const FNetBaseSubObjectCreationHeader* Base = static_cast<const FNetBaseSubObjectCreationHeader*>(Header);
	if (Base->IsDynamic())
	{
		const FNetDynamicSubObjectCreationHeader* H = static_cast<const FNetDynamicSubObjectCreationHeader*>(Header);
		AActor* Root = Cast<AActor>(Bridge->GetReplicatedObject(Context.RootObjectOfSubObject));
		if (Root && MusubiIsKeptAcrossRehome(Root))
		{
			UObject* Template = Bridge->ResolveObjectReference(H->TemplateReference, Context.ResolveContext);
			UObject* Outer = H->bOuterIsRootObject ? Root : (H->bOuterIsTransientLevel ? nullptr : Bridge->ResolveObjectReference(H->OuterReference, Context.ResolveContext));
			if (Template && Outer)
			{
				TArray<UObject*> Inner;
				GetObjectsWithOuter(Outer, Inner, /*bIncludeNestedObjects*/ false);
				UObject* Match = nullptr; int32 Matches = 0;
				// the client never registers replicated subobjects itself (AActor's list is the owner's), so the match is
				// outer + class + not yet attached to the new replication system
				for (UObject* O : Inner)
					if (IsValid(O) && O != Template && O->GetClass() == Template->GetClass() && !Bridge->GetReplicatedRefHandle(O).IsValid())
					{
						Match = O; ++Matches;
					}
				UE_LOG(LogMusubiIris, Verbose, TEXT("rehome: %d %s candidate(s) in %s for a dynamic subobject of %s"), Matches, *Template->GetClass()->GetName(), *Outer->GetName(), *Root->GetName());
				if (Matches == 1)
				{
					UE_LOG(LogMusubiIris, Log, TEXT("rehome: reused subobject %s of %s in place"), *Match->GetName(), *Root->GetName());
					FInstantiateResult Result;
					Result.Instance = Match;
					Result.Flags |= EReplicationBridgeCreateNetRefHandleResultFlags::AllowDestroyInstanceFromRemote;
					return Result;
				}
				if (Matches > 1) UE_LOG(LogMusubiIris, Warning, TEXT("rehome: %d %s subobjects of %s match, creating a new one"), Matches, *Template->GetClass()->GetName(), *Root->GetName());
			}
		}
	}
	return Super::InstantiateReplicatedObjectFromHeader(Context, Header);
}

#if ENGINE_MINOR_VERSION >= 8
void UMusubiSubObjectFactory::DetachedFromReplication(const FDetachContext& Context, const TOptional<FSubObjectDetachContext>& SubObjectContext)
{
	if (Context.Reason != EDetachReason::Stopped && Context.Reason != EDetachReason::TornOff && MusubiKeepSubObjectOnDetach(Context.DetachedInstance)) return;
	Super::DetachedFromReplication(Context, SubObjectContext);
}
#else
void UMusubiSubObjectFactory::DetachedFromReplication(const FDestroyedContext& Context)
{
	if (Context.DestroyReason != EReplicationBridgeDestroyInstanceReason::DoNotDestroy && Context.DestroyReason != EReplicationBridgeDestroyInstanceReason::TearOff && MusubiKeepSubObjectOnDetach(Context.DestroyedInstance)) return;
	Super::DetachedFromReplication(Context);
}
#endif
#endif // 5.7+
