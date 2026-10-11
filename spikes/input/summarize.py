# Print one table row per results/<name>.summary.json given on the command line.
import json, sys
def g(d, *ks):
    for k in ks:
        d = (d or {}).get(k)
    return d
print(f'{"run":20} {"e2e p50/p95/p99":>20} {"in p50/p95":>12} {"app>sub":>12} {"sub>pres":>12} {"ev/sim":>10} {"pos p95":>7} {"Δp max":>7} {"Δtilt":>6}')
for f in sys.argv[1:]:
    s = json.load(open(f)); L = s['latency_ms']
    pp = lambda k, qs=('p50','p95'): '/'.join(str(g(L,k,q)) for q in qs)
    a = s['accuracy']
    print(f'{s["name"].split("/")[-1]:20} {pp("e2e_sim_to_present",("p50","p95","p99")):>20} {pp("input_sim_to_app"):>12} {pp("app_to_submit"):>12} {pp("submit_to_present"):>12} '
          f'{str(s["contact_events"]["app_events"])+"/"+str(s["contact_events"]["sim_frames"]):>10} {str(g(a,"pos_err_px","p95")):>7} {str(g(a,"pressure_abs_err","max")):>7} {str(g(a,"tilt_abs_err_deg","max")):>6}')
