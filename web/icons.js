var u={xmlns:"http://www.w3.org/2000/svg",width:24,height:24,viewBox:"0 0 24 24",fill:"none",stroke:"currentColor","stroke-width":2,"stroke-linecap":"round","stroke-linejoin":"round"};var A=([e,a,t])=>{let r=document.createElementNS("http://www.w3.org/2000/svg",e);return Object.keys(a).forEach(o=>{r.setAttribute(o,String(a[o]))}),t?.length&&t.forEach(o=>{let s=A(o);r.appendChild(s)}),r},B=(e,a={})=>{let r={...u,...a};return A(["svg",r,e])};var M=e=>{for(let a in e)if(a.startsWith("aria-")||a==="role"||a==="title")return!0;return!1};var D=(...e)=>e.filter((a,t,r)=>!!a&&a.trim()!==""&&r.indexOf(a)===t).join(" ").trim();var F=e=>e.replace(/^([A-Z])|[\s-_]+(\w)/g,(a,t,r)=>r?r.toUpperCase():t.toLowerCase());var L=e=>{let a=F(e);return a.charAt(0).toUpperCase()+a.slice(1)};var U=e=>Array.from(e.attributes).reduce((a,t)=>(a[t.name]=t.value,a),{}),R=e=>typeof e=="string"?e:!e||!e.class?"":e.class&&typeof e.class=="string"?e.class.split(" "):e.class&&Array.isArray(e.class)?e.class:"",x=(e,{nameAttr:a,icons:t,attrs:r})=>{let o=e.getAttribute(a);if(o==null)return;let s=L(o),f=t[s];if(!f)return console.warn(`${e.outerHTML} icon name was not found in the provided icons object.`);let l=U(e),y=M(l)?{}:{"aria-hidden":"true"},k={...u,"data-lucide":o,...y,...r,...l},T=R(l),q=R(r),P=D("lucide",`lucide-${o}`,...T,...q);P&&Object.assign(k,{class:P});let b=B(f,k);return e.parentNode?.replaceChild(b,e)};var i=[["rect",{width:"20",height:"5",x:"2",y:"3",rx:"1"}],["path",{d:"M4 8v11a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8"}],["path",{d:"M10 12h4"}]];var n=[["path",{d:"m6 9 6 6 6-6"}]];var c=[["circle",{cx:"12",cy:"12",r:"10"}],["path",{d:"m16.24 7.76-1.804 5.411a2 2 0 0 1-1.265 1.265L7.76 16.24l1.804-5.411a2 2 0 0 1 1.265-1.265z"}]];var d=[["circle",{cx:"12",cy:"12",r:"1"}],["circle",{cx:"19",cy:"12",r:"1"}],["circle",{cx:"5",cy:"12",r:"1"}]];var C=[["path",{d:"M8 5h13"}],["path",{d:"M13 12h8"}],["path",{d:"M13 19h8"}],["path",{d:"M3 10a2 2 0 0 0 2 2h3"}],["path",{d:"M3 5v12a2 2 0 0 0 2 2h3"}]];var h=[["path",{d:"m16 6-8.414 8.586a2 2 0 0 0 2.829 2.829l8.414-8.586a4 4 0 1 0-5.657-5.657l-8.379 8.551a6 6 0 1 0 8.485 8.485l8.379-8.551"}]];var p=[["path",{d:"M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"}],["path",{d:"M3 3v5h5"}],["path",{d:"M12 7v5l4 2"}]];var m=[["path",{d:"M3.714 3.048a.498.498 0 0 0-.683.627l2.843 7.627a2 2 0 0 1 0 1.396l-2.842 7.627a.498.498 0 0 0 .682.627l18-8.5a.5.5 0 0 0 0-.904z"}],["path",{d:"M6 12h16"}]];var S=[["path",{d:"M9.671 4.136a2.34 2.34 0 0 1 4.659 0 2.34 2.34 0 0 0 3.319 1.915 2.34 2.34 0 0 1 2.33 4.033 2.34 2.34 0 0 0 0 3.831 2.34 2.34 0 0 1-2.33 4.033 2.34 2.34 0 0 0-3.319 1.915 2.34 2.34 0 0 1-4.659 0 2.34 2.34 0 0 0-3.32-1.915 2.34 2.34 0 0 1-2.33-4.033 2.34 2.34 0 0 0 0-3.831A2.34 2.34 0 0 1 6.35 6.051a2.34 2.34 0 0 0 3.319-1.915"}],["circle",{cx:"12",cy:"12",r:"3"}]];var g=[["rect",{width:"8",height:"8",x:"3",y:"3",rx:"2"}],["path",{d:"M7 11v4a2 2 0 0 0 2 2h4"}],["rect",{width:"8",height:"8",x:"13",y:"13",rx:"2"}]];var w=({icons:e={},nameAttr:a="data-lucide",attrs:t={},root:r=document,inTemplates:o}={})=>{if(!Object.values(e).length)throw new Error(`Please provide an icons object.
If you want to use all the icons you can import it like:
 \`import { createIcons, icons } from 'lucide';
lucide.createIcons({icons});\``);if(typeof r>"u")throw new Error("`createIcons()` only works in a browser environment.");if(Array.from(r.querySelectorAll(`[${a}]`)).forEach(f=>x(f,{nameAttr:a,icons:e,attrs:t})),o&&Array.from(r.querySelectorAll("template")).forEach(l=>w({icons:e,nameAttr:a,attrs:t,root:l.content,inTemplates:o})),a==="data-lucide"){let f=r.querySelectorAll("[icon-name]");f.length>0&&(console.warn("[Lucide] Some icons were found with the now deprecated icon-name attribute. These will still be replaced for backwards compatibility, but will no longer be supported in v1.0 and you should switch to data-lucide"),Array.from(f).forEach(l=>x(l,{nameAttr:"icon-name",icons:e,attrs:t})))}};var v={Archive:i,ChevronDown:n,Compass:c,Ellipsis:d,History:p,ListTree:C,Paperclip:h,SendHorizontal:m,Settings:S,Workflow:g};function ge(e=document){w({icons:v,root:e,inTemplates:!0,attrs:{"aria-hidden":"true",focusable:"false","stroke-width":"1.8"}})}export{ge as renderIcons};
/*! Bundled license information:

lucide/dist/esm/defaultAttributes.mjs:
lucide/dist/esm/createElement.mjs:
lucide/dist/esm/shared/src/utils/hasA11yProp.mjs:
lucide/dist/esm/shared/src/utils/mergeClasses.mjs:
lucide/dist/esm/shared/src/utils/toCamelCase.mjs:
lucide/dist/esm/shared/src/utils/toPascalCase.mjs:
lucide/dist/esm/replaceElement.mjs:
lucide/dist/esm/icons/archive.mjs:
lucide/dist/esm/icons/chevron-down.mjs:
lucide/dist/esm/icons/compass.mjs:
lucide/dist/esm/icons/ellipsis.mjs:
lucide/dist/esm/icons/list-tree.mjs:
lucide/dist/esm/icons/paperclip.mjs:
lucide/dist/esm/icons/rotate-ccw-clock.mjs:
lucide/dist/esm/icons/send-horizontal.mjs:
lucide/dist/esm/icons/settings.mjs:
lucide/dist/esm/icons/workflow.mjs:
lucide/dist/esm/lucide.mjs:
  (**
   * @license lucide v1.28.0 - ISC
   *
   * This source code is licensed under the ISC license.
   * See the LICENSE file in the root directory of this source tree.
   *)
*/
