// SDK-typed activation boundary. No shell broker, app, station or COM apartment
// is changed by these exports; only supplied values and owned objects are used.
#define CINTERFACE
#define COBJMACROS
#include <windows.h>
#include <shobjidl.h>
#include <cstddef>
#include <string>
static_assert(sizeof(void*)==8 && sizeof(USEROBJECTFLAGS)==12);
static_assert(offsetof(USEROBJECTFLAGS,dwFlags)==8);
static_assert(offsetof(IApplicationActivationManagerVtbl,ActivateApplication)==3*sizeof(void*));
static_assert(offsetof(IApplicationActivationManagerVtbl,ActivateForFile)==4*sizeof(void*));
static_assert(offsetof(IApplicationActivationManagerVtbl,ActivateForProtocol)==5*sizeof(void*));
static const GUID classId={0x45ba127d,0x10a8,0x46ea,{0x8a,0xb7,0x56,0xea,0x90,0x78,0x94,0x3c}};
static const GUID interfaceId={0x2e941141,0x7f97,0x4756,{0xba,0x1d,0x9d,0xec,0xde,0x89,0x4a,0x3d}};
static const GUID unknownId={0,0,0,{0xc0,0,0,0,0,0,0,0x46}};
struct Manager {IApplicationActivationManagerVtbl* table;LONG refs;};
static Manager manager{};
static std::string trace,snapshot;
static HRESULT initializeResult,createResult,activateResult;
static DWORD processId,windowFlags;
static int outputMode,stationPresent,informationResult;
static HRESULT STDMETHODCALLTYPE query(IApplicationActivationManager* self,REFIID iid,void** out){
 *out=nullptr;if(iid!=unknownId&&iid!=interfaceId)return E_NOINTERFACE;
 *out=self;++manager.refs;return S_OK;
}
static ULONG STDMETHODCALLTYPE add(IApplicationActivationManager*){return ++manager.refs;}
static ULONG STDMETHODCALLTYPE release(IApplicationActivationManager*){
 LONG refs=--manager.refs;if(refs==0)trace+="released;";return refs;
}
static std::string text(LPCWSTR value){if(!value)return "null";std::string out;for(;*value;value++)out+=static_cast<char>(*value);return out;}
static HRESULT STDMETHODCALLTYPE activate(IApplicationActivationManager*,LPCWSTR id,LPCWSTR args,ACTIVATEOPTIONS options,DWORD* pid){
 trace+="activate:"+text(id)+":"+text(args)+":"+std::to_string(options)+":"+std::to_string(pid!=nullptr)+";";
 if(pid)*pid=processId;return activateResult;
}
static HRESULT STDMETHODCALLTYPE file(IApplicationActivationManager*,LPCWSTR,IShellItemArray*,LPCWSTR,DWORD*){trace+="unexpected_file;";return E_NOTIMPL;}
static HRESULT STDMETHODCALLTYPE protocol(IApplicationActivationManager*,LPCWSTR,IShellItemArray*,DWORD*){trace+="unexpected_protocol;";return E_NOTIMPL;}
static IApplicationActivationManagerVtbl table={query,add,release,activate,file,protocol};
extern "C" __declspec(dllexport) void FixtureReset(int init,int create,int act,DWORD pid,int output,int station,int information,DWORD flags){
 initializeResult=init;createResult=create;activateResult=act;processId=pid;outputMode=output;stationPresent=station;informationResult=information;windowFlags=flags;manager={&table,0};trace.clear();
}
extern "C" __declspec(dllexport) HWINSTA WINAPI FixtureStation(){trace+="station;";return stationPresent?reinterpret_cast<HWINSTA>(0x1234):nullptr;}
extern "C" __declspec(dllexport) BOOL WINAPI FixtureInformation(HANDLE station,int index,PVOID value,DWORD size,LPDWORD needed){
 trace+="information:"+std::to_string(station==reinterpret_cast<HANDLE>(0x1234))+":"+std::to_string(index)+":"+std::to_string(size)+":"+std::to_string(needed!=nullptr)+";";
 if(size==sizeof(USEROBJECTFLAGS)&&value){auto flags=static_cast<USEROBJECTFLAGS*>(value);trace+="flags:"+std::to_string(flags->fInherit)+":"+std::to_string(flags->fReserved)+":"+std::to_string(flags->dwFlags)+";";flags->dwFlags=windowFlags;}
 if(needed)*needed=sizeof(USEROBJECTFLAGS);return informationResult;
}
// Independent SDK expression for the documented Environment.UserInteractive
// contract. Its supported-runtime control flow is inspected separately in IL.
extern "C" __declspec(dllexport) BOOL FixtureInteractive(){
 USEROBJECTFLAGS flags{};DWORD needed=0;auto station=FixtureStation();
 return !station||!FixtureInformation(station,UOI_FLAGS,&flags,sizeof(flags),&needed)||(flags.dwFlags&WSF_VISIBLE)!=0;
}
extern "C" __declspec(dllexport) HRESULT WINAPI FixtureInitialize(PVOID reserved,DWORD mode){trace+="initialize:"+std::to_string(reserved==nullptr)+":"+std::to_string(mode)+";";return initializeResult;}
extern "C" __declspec(dllexport) void WINAPI FixtureUninitialize(){trace+="uninitialize;";}
extern "C" __declspec(dllexport) HRESULT WINAPI FixtureCreate(REFCLSID clsid,IUnknown* outer,DWORD context,REFIID iid,PVOID* out){
 trace+="create:"+std::to_string(clsid==classId)+":"+std::to_string(outer==nullptr)+":"+std::to_string(context)+":"+std::to_string(iid==interfaceId)+":"+std::to_string(out!=nullptr)+";";
 *out=nullptr;if(outputMode){manager.refs=1;*out=&manager;}return createResult;
}
extern "C" __declspec(dllexport) const char* FixtureTrace(){return trace.c_str();}
extern "C" __declspec(dllexport) int FixtureReferences(){return manager.refs;}
