// Run against an isolated local Compose instance, never a production deployment.
// MOEFLOW_TEST_URL=http://127.0.0.1:8081 node backend-rust/tests/frontend_contracts.cjs
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const zlib = require('node:zlib');

const base = process.env.MOEFLOW_TEST_URL;
assert(base && /^http:\/\/(127\.0\.0\.1|localhost):\d+$/.test(base), 'Set MOEFLOW_TEST_URL to an isolated local instance');
const env = Object.fromEntries(fs.readFileSync(path.join(__dirname, '../.env'), 'utf8').split(/\r?\n/).filter(v => /^[A-Z_]+=/.test(v)).map(v => [v.slice(0, v.indexOf('=')), v.slice(v.indexOf('=') + 1)]));
const stamp = Date.now();
const password = 'ContractPass123!';
const digits = [
  [14,17,19,21,25,17,14], [4,12,4,4,4,4,14], [14,17,1,2,4,8,31],
  [30,1,1,14,1,1,30], [2,6,10,18,31,2,2], [31,16,16,30,1,1,30],
  [14,16,16,30,17,17,14], [31,1,2,4,8,8,8], [14,17,17,14,17,17,14], [14,17,17,15,1,1,14],
];

// Decode only this application's local test CAPTCHA raster; tests still exercise
// the real challenge, attempt consumption, authentication, and session creation.
function decodeCaptcha(image) {
  const png = Buffer.from(image.split(',')[1], 'base64');
  let width, height, offset = 8;
  const chunks = [];
  while (offset < png.length) {
    const size = png.readUInt32BE(offset);
    const type = png.toString('ascii', offset + 4, offset + 8);
    if (type === 'IHDR') {
      width = png.readUInt32BE(offset + 8); height = png.readUInt32BE(offset + 12);
      assert.equal(png[offset + 16], 8); assert.equal(png[offset + 17], 2);
    }
    if (type === 'IDAT') chunks.push(png.subarray(offset + 8, offset + 8 + size));
    offset += size + 12;
  }
  const raw = zlib.inflateSync(Buffer.concat(chunks));
  const stride = width * 3;
  const pixels = Buffer.alloc(height * stride);
  const paeth = (a,b,c) => { const p=a+b-c; const x=Math.abs(p-a),y=Math.abs(p-b),z=Math.abs(p-c); return x<=y&&x<=z?a:y<=z?b:c; };
  for (let y=0;y<height;y++) {
    const filter = raw[y*(stride+1)];
    for (let x=0;x<stride;x++) {
      const at=y*stride+x, a=x>=3?pixels[at-3]:0, b=y?pixels[at-stride]:0, c=y&&x>=3?pixels[at-stride-3]:0;
      const predictor = [0,a,b,Math.floor((a+b)/2),paeth(a,b,c)][filter];
      pixels[at]=(raw[y*(stride+1)+1+x]+predictor)&255;
    }
  }
  let answer = '';
  for (let i=0;i<6;i++) {
    let best = {score:Infinity,digit:0};
    for (let d=0;d<10;d++) for (let top=4;top<12;top++) {
      let score=0;
      for (let y=0;y<7;y++) for (let x=0;x<5;x++) {
        let ink=0;
        for (let dy=0;dy<4;dy++) for (let dx=0;dx<4;dx++) if (pixels[((top+y*4+dy)*width+7+i*25+x*4+dx)*3]<80) ink++;
        score+=((digits[d][y]&(1<<(4-x)))?16-ink:ink);
      }
      if(score<best.score) best={score,digit:d};
    }
    answer+=best.digit;
  }
  return answer;
}

async function api(method, url, token, data, expected=200) {
  const response = await fetch(base+'/api/v1'+url, {method, signal:AbortSignal.timeout(15000), headers:{...(token?{Authorization:'Bearer '+token}:{}),...(data===undefined?{}:{'Content-Type':'application/json'})},body:data===undefined?undefined:JSON.stringify(data)});
  const body = await response.json();
  assert.equal(response.status, expected, method+' '+url+': '+JSON.stringify(body));
  return {body,headers:response.headers};
}
async function challenge() {
  const {body} = await api('POST','/captchas');
  return {captcha_info:body.info,captcha:decodeCaptcha(body.image)};
}
async function login(email, pass) {
  return (await api('POST','/user/token',undefined,{email,password:pass,...await challenge()})).body.token;
}
async function pending(url, token, count) {
  const {body,headers}=await api('GET',url,token);
  assert.equal(Number(headers.get('x-pagination-count')),count,url);
  assert.equal(body.length,Math.min(count,1),url);
  return body;
}

(async () => {
  const admin = await login(env.ADMIN_EMAIL,env.ADMIN_PASSWORD);
  const users=[];
  for (const name of ['owner','joiner','applicant']) {
    const email=`contract-${stamp}-${name}@example.test`;
    const {body}=await api('POST','/admin/users',admin,{email,password,name:`Contract_${name}_${stamp}`});
    users.push({...body,email,token:await login(email,password)});
  }
  const [owner,joiner,applicant]=users;
  const team=(await api('POST','/teams',owner.token,{name:`Contract_${stamp}`,allow_apply_type:2,application_check_type:2,default_role:'team-member'})).body.team;
  const set=(await api('POST',`/teams/${team.id}/project-sets`,owner.token,{name:'Contract set',intro:''})).body.project_set;
  const project=(await api('POST',`/teams/${team.id}/projects`,owner.token,{name:'Contract project',project_set:set.id,source_language:'ja',target_languages:['zh-TW'],allow_apply_type:2,application_check_type:2,default_role:'project-supporter'})).body.project;

  const target=(await api('POST',`/projects/${project.id}/targets`,owner.token,{language:'en'})).body;
  assert(target.message && target.target?.id); assert.equal(target.target.language.id,'en');
  await api('POST',`/projects/${project.id}/targets`,owner.token,{language:'en'},400);
  console.log('PASS: target creation response and duplicate prevention');

  const invitation=(await api('POST',`/teams/${team.id}/invitations`,owner.token,{user_id:joiner.id,role_id:'team-member',message:'reason, not a success notification'})).body;
  assert(invitation.invitation?.id); assert.notEqual(invitation.message,invitation.invitation.message);
  const edited=(await api('PUT',`/invitations/${invitation.invitation.id}`,owner.token,{role_id:'team-beginner'})).body;
  assert.equal(edited.invitation.role.id,'team-beginner'); assert(edited.message);
  await pending('/user/invitations?status=0&limit=1',joiner.token,1);
  const accepted=(await api('PATCH',`/invitations/${invitation.invitation.id}`,joiner.token,{allow:true})).body;
  assert(accepted.invitation && accepted.group.role && accepted.message);
  await pending('/user/invitations?status=0&limit=1',joiner.token,0);
  await pending('/user/invitations?status=1&limit=1',joiner.token,1);
  await pending('/user/invitations?status=0&status=1&limit=1',joiner.token,1);
  await api('GET','/user/invitations?status=99',joiner.token,undefined,400);
  console.log('PASS: invite create/edit/accept and pending counts after processing');

  const application=(await api('POST',`/teams/${team.id}/applications`,applicant.token,{message:'application reason'})).body;
  assert.equal(application.joined,false); assert(application.application.id && application.group.id);
  await pending('/user/related-applications?status=0&limit=1',applicant.token,0);
  await pending('/user/related-applications?status=0&limit=1',owner.token,1);
  await api('PATCH',`/applications/${application.application.id}`,owner.token,{allow:true});
  await pending('/user/related-applications?status=0&limit=1',owner.token,0);
  const openTeam=(await api('POST','/teams',owner.token,{name:`Open_${stamp}`,allow_apply_type:2,application_check_type:1,default_role:'team-member'})).body.team;
  const joined=(await api('POST',`/teams/${openTeam.id}/applications`,joiner.token,{message:''})).body;
  assert.equal(joined.joined,true); assert(joined.group.role && joined.message);
  console.log('PASS: application processing, reviewer scope, and pending counts');

  const address=`captcha-${stamp}@example.test`;
  const wrong=await challenge(); wrong.captcha=String((Number(wrong.captcha)+1)%1000000).padStart(6,'0');
  await api('POST','/confirm-email-codes',undefined,{email:address,...wrong},400);
  await api('POST','/confirm-email-codes',undefined,{email:address,...await challenge()});
  const cooled=await api('POST','/confirm-email-codes',undefined,{email:address,...await challenge()},429);
  assert(cooled.body.retry_after>0&&cooled.body.retry_after<=60);
  assert.equal(Number(cooled.headers.get('retry-after')),cooled.body.retry_after);
  console.log('PASS: wrong CAPTCHA, fresh-challenge recovery, and structured cooldown');

  for (const length of [6,7,129]) {
    const invalid=(await api('POST','/users',undefined,{email:`invalid-${stamp}@example.test`,name:'Invalid',password:'a'.repeat(length)},400)).body;
    assert.match(invalid.message,/8.*128/);
  }
  const longUser=(await api('POST','/admin/users',admin,{email:`long-${stamp}@example.test`,name:`Contract_long_${stamp}`,password:'a'.repeat(128)})).body;
  assert(longUser.id); await login(`long-${stamp}@example.test`,'a'.repeat(128));
  console.log('PASS: API password lower/upper boundaries and long-password login');

  for (const kind of ['teams','projects']) {
    const missing=await api('GET',`/${kind}/00000000-0000-4000-8000-000000000000/public-info`,owner.token,undefined,404);
    assert.equal(missing.body.code,404);
  }
  console.log('PASS: missing team/project IDs return structured 404 errors');
  if (process.env.MOEFLOW_TEST_SESSION_FILE) fs.writeFileSync(process.env.MOEFLOW_TEST_SESSION_FILE,JSON.stringify({base,team,project,target,owner,joiner,applicant,password}));
  console.log('All API contract regression checks passed');
})().catch(error=>{console.error(error.message);process.exitCode=1;});
