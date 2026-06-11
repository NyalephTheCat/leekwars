import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_384 extends AI {
public AI_384() throws LeekRunException {
super(2, 1);
}
public void staticInit() throws LeekRunException {
}
public Object runIA(Session session) throws LeekRunException {
var u_ax = new Box<Object>(AI_384.this, Number_randInt_ii(-17l, 17l), 31);
var u_ay = new Box<Object>(AI_384.this, Number_randInt_ii((Object) add(-17l, Number_abs_r_i_l(load(u_ax.get()))), (Object) sub(17l, Number_abs_r_i_l(load(u_ax.get())))), 37);
return null;
}
protected String getAIString() { return "<snippet 384>";}
protected String[] getErrorFiles() { return new String[] {"<snippet 384>", };}

protected int[] getErrorFilesID() { return new int[] {384, };}

private Object Number_abs_r_i_l(Object a0) throws LeekRunException {
if (a0 instanceof Long x0) {
return NumberClass.abs(this, (Long) a0);
}
if (a0 instanceof BigIntegerValue x0) {
return NumberClass.abs(this, (BigIntegerValue) a0);
}
double x0; try { x0 = real(a0); } catch (ClassCastException e) { return 0.0; }
return NumberClass.abs(this, x0);
}

private long Number_randInt_ii(Object a0, Object a1) throws LeekRunException {
long x0; try { x0 = longint(a0); } catch (ClassCastException e) { return 0l; }
long x1; try { x1 = longint(a1); } catch (ClassCastException e) { return 0l; }
return NumberClass.randInt(this, x0, x1);
}

}
