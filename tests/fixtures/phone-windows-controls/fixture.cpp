// SDK-typed, process-owned COM fixture. No installed app or accessibility provider.
#define CINTERFACE
#define COBJMACROS
#include <windows.h>
#include <UIAutomationClient.h>
#include <oleauto.h>
#include <cstddef>
#include <string>
#include <vector>
#include <algorithm>
#define SLOT(type, field, slot) static_assert(offsetof(type, field) == (slot)*sizeof(void*))
SLOT(IUIAutomation2Vtbl, ElementFromHandle, 6);
SLOT(IUIAutomation2Vtbl, CreateTreeWalker, 13);
SLOT(IUIAutomation2Vtbl, get_RawViewWalker, 16);
SLOT(IUIAutomation2Vtbl, CreateCacheRequest, 20);
SLOT(IUIAutomation2Vtbl, CreatePropertyCondition, 23);
SLOT(IUIAutomation2Vtbl, put_AutoSetFocus, 59);
SLOT(IUIAutomation2Vtbl, put_ConnectionTimeout, 61);
SLOT(IUIAutomation2Vtbl, put_TransactionTimeout, 63);
SLOT(IUIAutomationElementVtbl, FindAllBuildCache, 8);
SLOT(IUIAutomationElementVtbl, BuildUpdatedCache, 9);
SLOT(IUIAutomationElementVtbl, GetCurrentPropertyValueEx, 11);
SLOT(IUIAutomationElementVtbl, GetCachedPropertyValueEx, 13);
SLOT(IUIAutomationElementVtbl, GetCurrentPatternAs, 14);
SLOT(IUIAutomationTreeWalkerVtbl, NormalizeElement, 8);
SLOT(IUIAutomationTreeWalkerVtbl, GetParentElementBuildCache, 9);
static_assert(sizeof(VARIANT)==24 && offsetof(VARIANT,lVal)==8);
struct Object { void* table; LONG refs=1; int id; };
static IUIAutomation2Vtbl automationTable{};
static IUIAutomationElementVtbl elementTable{};
static IUIAutomationElementArrayVtbl arrayTable{};
static IUIAutomationCacheRequestVtbl cacheTable{};
static IUIAutomationTreeWalkerVtbl walkerTable{};
static IUIAutomationInvokePatternVtbl invokeTable{};
static IUnknownVtbl unknownTable{};
static Object automation, cache[2], condition[2], walker[2], array, element[70], pattern, unknown;
static std::vector<Object*> objects;
static std::vector<HWND> windows;
static std::string trace;
static int mode=0, phase=0, calls=0, failure=-1, hideAt=-1, caches=0;
static HRESULT failureHr=E_ACCESSDENIED;
static HRESULT tick(std::string item) {
    trace+=item+";";
    int call=calls++;
    if(call==hideAt && !windows.empty()) ShowWindow(windows[0],SW_HIDE);
    return call==failure?failureHr:S_OK;
}
template<class T> static ULONG STDMETHODCALLTYPE add(T* self) { return ++reinterpret_cast<Object*>(self)->refs; }
template<class T> static ULONG STDMETHODCALLTYPE release(T* self) { return --reinterpret_cast<Object*>(self)->refs; }
template<class T> static HRESULT STDMETHODCALLTYPE query(T* self, REFIID iid, void** out) {
    *out=nullptr;
    static const GUID a={0x34723aff,0x0c9d,0x49d0,{0x98,0x96,0x7a,0xb5,0x2d,0xf8,0xcd,0x8a}};
    static const GUID e={0xd22108aa,0x8ac5,0x49a5,{0x83,0x7b,0x37,0xbb,0xb3,0xd7,0x59,0x1e}};
    static const GUID w={0x4042c624,0x389c,0x4afc,{0xa6,0x30,0x9d,0xf8,0x54,0xa5,0x41,0xfc}};
    static const GUID c={0xb32a92b5,0xbc25,0x4078,{0x9c,0x08,0xd7,0xee,0x95,0xc4,0x8e,0x03}};
    static const GUID r={0x14314595,0xb4bc,0x4055,{0x95,0xf2,0x58,0xf2,0xe4,0x2c,0x98,0x55}};
    static const GUID i={0xfb377fbe,0x8ea6,0x46d5,{0x9c,0x73,0x64,0x99,0x64,0x2d,0x30,0x59}};
    static const GUID u={0,0,0,{0xc0,0,0,0,0,0,0,0x46}};
    auto object=reinterpret_cast<Object*>(self);
    const GUID* own=object->table==&automationTable?&a:object->table==&elementTable?&e:object->table==&walkerTable?&w:object->table==&cacheTable?&c:object->table==&arrayTable?&r:object->table==&invokeTable?&i:&u;
    if(iid!=u && iid!=*own) return E_NOINTERFACE;
    *out=self;add(self);return S_OK;
}
template<class T> static HRESULT give(Object& object,T** out,HRESULT hr) { *out=nullptr;if(hr!=S_OK)return hr;++object.refs;*out=reinterpret_cast<T*>(&object);return S_OK; }
static std::string number(int n){return std::to_string(n);}
static int id(void* self){return reinterpret_cast<Object*>(self)->id;}
static int ordinal(UIA_HWND window){auto it=std::find(windows.begin(),windows.end(),reinterpret_cast<HWND>(window));return it==windows.end()?-1:static_cast<int>(it-windows.begin());}
static HRESULT STDMETHODCALLTYPE elementFrom(IUIAutomation2*,UIA_HWND window,IUIAutomationElement** out){return give(element[0],out,tick("element:"+number(ordinal(window))));}
static HRESULT STDMETHODCALLTYPE createCache(IUIAutomation2*,IUIAutomationCacheRequest** out){int index=caches++%2;return give(cache[index],out,tick("cache:"+number(index)));}
static HRESULT STDMETHODCALLTYPE propertyCondition(IUIAutomation2*,PROPERTYID property,VARIANT value,IUIAutomationCondition** out){int index=property==30003?0:1;std::string text=property==30020?"window":number(value.lVal);return give(condition[index],out,tick("condition:"+number(property)+":"+number(value.vt)+":"+text));}
static HRESULT STDMETHODCALLTYPE tree(IUIAutomation2*,IUIAutomationCondition* value,IUIAutomationTreeWalker** out){return give(walker[1],out,tick("tree:"+number(id(value))));}
static HRESULT STDMETHODCALLTYPE rawWalker(IUIAutomation2*,IUIAutomationTreeWalker** out){return give(walker[0],out,tick("raw"));}
static HRESULT STDMETHODCALLTYPE focus(IUIAutomation2*,BOOL value){return tick("focus:"+number(value));}
static HRESULT STDMETHODCALLTYPE connection(IUIAutomation2*,DWORD value){return tick("connection:"+number(value));}
static HRESULT STDMETHODCALLTYPE transaction(IUIAutomation2*,DWORD value){return tick("transaction:"+number(value));}
static HRESULT STDMETHODCALLTYPE addProperty(IUIAutomationCacheRequest* self,PROPERTYID property){return tick("add:"+number(id(self))+":"+number(property));}
static HRESULT STDMETHODCALLTYPE findAll(IUIAutomationElement* self,TreeScope scope,IUIAutomationCondition* match,IUIAutomationCacheRequest* c,IUIAutomationElementArray** out){return give(array,out,tick("find:"+number(id(self))+":"+number(scope)+":"+number(id(match))+":"+number(id(c))));}
static HRESULT STDMETHODCALLTYPE length(IUIAutomationElementArray*,int* out){auto hr=tick("length");*out=mode==10?-1:mode==11?4097:mode==12?65:3;return hr;}
static HRESULT STDMETHODCALLTYPE at(IUIAutomationElementArray*,int index,IUIAutomationElement** out){return give(element[1+index],out,tick("at:"+number(index)));}
static HRESULT STDMETHODCALLTYPE update(IUIAutomationElement* self,IUIAutomationCacheRequest* c,IUIAutomationElement** out){return give(*reinterpret_cast<Object*>(self),out,tick("update:"+number(id(self))+":"+number(id(c))));}
static HRESULT STDMETHODCALLTYPE parent(IUIAutomationTreeWalker* self,IUIAutomationElement* e,IUIAutomationCacheRequest* c,IUIAutomationElement** out){return give(element[69],out,tick("parent:"+number(id(self))+":"+number(id(e))+":"+number(id(c))));}
static HRESULT STDMETHODCALLTYPE normalize(IUIAutomationTreeWalker* self,IUIAutomationElement* e,IUIAutomationElement** out){return give(element[0],out,tick("normalize:"+number(id(self))+":"+number(id(e))));}
static void integers(VARIANT* out,int value,LONG lower=0){out->vt=VT_ARRAY|VT_I4;out->parray=SafeArrayCreateVector(VT_I4,lower,2);LONG* p=nullptr;SafeArrayAccessData(out->parray,reinterpret_cast<void**>(&p));p[0]=42;p[1]=value;SafeArrayUnaccessData(out->parray);}
static void text(VARIANT* out,const wchar_t* value){out->vt=VT_BSTR;out->bstrVal=SysAllocString(value);}
static HRESULT getProperty(IUIAutomationElement* self,PROPERTYID property,BOOL ignore,VARIANT* out,bool cached){
    VariantInit(out);int n=id(self);auto hr=tick(std::string(cached?"cached:":"current:")+number(n)+":"+number(property)+":"+number(ignore));if(hr!=S_OK)return hr;
    if(property==30000){if(mode==3&&n==1){out->vt=VT_I4;out->lVal=1;}else integers(out,phase==1&&mode==1&&n==1?99:n==69?(phase==1&&mode==2?99:0):n,mode==4?1:0);}
    if(property==30005){if(mode==5&&n==1){out->vt=VT_I4;out->lVal=1;}else text(out,mode==12||n==1?(phase==1&&mode==4?L"Changed":L"Mute microphone"):n==2?L"Stop voice chat":L"Other");}
    if(property==30010){if(mode==6&&n==1)text(out,L"true");else{out->vt=VT_BOOL;out->boolVal=phase==1&&mode==5&&n==1?VARIANT_FALSE:VARIANT_TRUE;}}
    if(property==30022){out->vt=VT_BOOL;out->boolVal=(phase==1&&mode==6&&n==1)||(mode==13&&n==2)?VARIANT_TRUE:VARIANT_FALSE;}
    if(property==30086){if(mode==7||phase==1&&mode==7){out->vt=VT_UNKNOWN;out->punkVal=reinterpret_cast<IUnknown*>(&unknown);add(out->punkVal);}else if(n==1){out->vt=VT_I4;out->lVal=0;}}
    if(property==30020){out->vt=VT_I4;out->lVal=static_cast<LONG>(reinterpret_cast<INT_PTR>(windows[0]))+(phase==1&&mode==3?1:0);}
    return S_OK;
}
static HRESULT STDMETHODCALLTYPE cached(IUIAutomationElement* self,PROPERTYID property,BOOL ignore,VARIANT* out){return getProperty(self,property,ignore,out,true);}
static HRESULT STDMETHODCALLTYPE current(IUIAutomationElement* self,PROPERTYID property,BOOL ignore,VARIANT* out){return getProperty(self,property,ignore,out,false);}
static HRESULT STDMETHODCALLTYPE getPattern(IUIAutomationElement* self,PATTERNID p,REFIID iid,void** out){static const GUID expected={0xfb377fbe,0x8ea6,0x46d5,{0x9c,0x73,0x64,0x99,0x64,0x2d,0x30,0x59}};return give(pattern,out,tick("pattern:"+number(id(self))+":"+number(p)+":"+(iid==expected?"ok":"wrong")));}
static HRESULT STDMETHODCALLTYPE invoke(IUIAutomationInvokePattern*){auto hr=tick("invoke");return mode==14?E_FAIL:mode==15?S_FALSE:hr;}
template<class V,class T> static void base(V& table){table.QueryInterface=&query<T>;table.AddRef=&add<T>;table.Release=&release<T>;}
static void object(Object& value,void* table,int id){value={table,1,id};objects.push_back(&value);}
extern "C" __declspec(dllexport) int FixtureReset(int value,int count){
    for(auto window:windows)DestroyWindow(window);windows.clear();objects.clear();trace.clear();mode=value;phase=0;calls=0;failure=-1;hideAt=-1;caches=0;
    base<IUIAutomation2Vtbl,IUIAutomation2>(automationTable);automationTable.ElementFromHandle=elementFrom;automationTable.CreateCacheRequest=createCache;automationTable.CreatePropertyCondition=propertyCondition;automationTable.CreateTreeWalker=tree;automationTable.get_RawViewWalker=rawWalker;automationTable.put_AutoSetFocus=focus;automationTable.put_ConnectionTimeout=connection;automationTable.put_TransactionTimeout=transaction;
    base<IUIAutomationElementVtbl,IUIAutomationElement>(elementTable);elementTable.FindAllBuildCache=findAll;elementTable.BuildUpdatedCache=update;elementTable.GetCachedPropertyValueEx=cached;elementTable.GetCurrentPropertyValueEx=current;elementTable.GetCurrentPatternAs=getPattern;
    base<IUIAutomationElementArrayVtbl,IUIAutomationElementArray>(arrayTable);arrayTable.get_Length=length;arrayTable.GetElement=at;
    base<IUIAutomationCacheRequestVtbl,IUIAutomationCacheRequest>(cacheTable);cacheTable.AddProperty=addProperty;
    base<IUIAutomationTreeWalkerVtbl,IUIAutomationTreeWalker>(walkerTable);walkerTable.GetParentElementBuildCache=parent;walkerTable.NormalizeElement=normalize;
    base<IUIAutomationInvokePatternVtbl,IUIAutomationInvokePattern>(invokeTable);invokeTable.Invoke=invoke;base<IUnknownVtbl,IUnknown>(unknownTable);
    object(automation,&automationTable,0);object(array,&arrayTable,0);object(pattern,&invokeTable,0);object(unknown,&unknownTable,0);
    for(int n=0;n<2;n++){object(cache[n],&cacheTable,n);object(condition[n],&unknownTable,n);object(walker[n],&walkerTable,n);}
    for(int n=0;n<70;n++)object(element[n],&elementTable,n);
    for(int n=0;n<count;n++){auto window=CreateWindowExW(WS_EX_NOACTIVATE|WS_EX_TOOLWINDOW,L"STATIC",L"",WS_POPUP|WS_VISIBLE,-30000,-30000,1,1,nullptr,nullptr,GetModuleHandleW(nullptr),nullptr);if(!window)return 0;windows.push_back(window);}std::sort(windows.begin(),windows.end());return 1;
}
extern "C" __declspec(dllexport) void* FixtureAutomation(){++automation.refs;return &automation;}
extern "C" __declspec(dllexport) void FixtureFailure(int index,int hr,int hide){failure=index;failureHr=hr;hideAt=hide;}
extern "C" __declspec(dllexport) void FixturePhase(int value){phase=value;if(value==1&&mode==8)ShowWindow(windows[0],SW_HIDE);if(value==1&&mode==9)ShowWindow(windows[0],SW_SHOWMINNOACTIVE);}
extern "C" __declspec(dllexport) const char* FixtureTrace(){return trace.c_str();}
extern "C" __declspec(dllexport) int FixtureReferences(){int refs=0;for(auto object:objects)refs+=object->refs-1;return refs;}
extern "C" __declspec(dllexport) int FixtureWindowOrdinal(INT_PTR window){return ordinal(reinterpret_cast<UIA_HWND>(window));}
extern "C" __declspec(dllexport) void FixtureDestroy(){for(auto window:windows)DestroyWindow(window);windows.clear();}
