import datetime as dt, nightdisplay as n
D=lambda h,m: dt.datetime(2026,1,1,h,m)
for (h,m),exp in {(0,0):1,(10,29):1,(10,30):0,(19,59):0,(20,0):1,(23,59):1}.items():
    assert n.is_night("20:00","10:30",D(h,m))==bool(exp),(h,m)
assert n.is_night("09:00","17:00",D(12,0)) and not n.is_night("09:00","17:00",D(17,0))
assert not n.is_night("08:00","08:00",D(8,0))
print("logic ok")
