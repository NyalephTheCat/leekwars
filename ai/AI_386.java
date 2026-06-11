import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_386 extends AI {
public AI_386() throws LeekRunException {
super(2, 3);
}
public void staticInit() throws LeekRunException {
}
public Object runIA(Session session) throws LeekRunException {
Object u_ax = ops(Number_randInt_ii(-17l, 17l), 32);
Object u_ay = ops(Number_randInt_ii((Object) add(-17l, Number_abs_r_i_l(u_ax)), (Object) sub(17l, Number_abs_r_i_l(u_ax))), 38);
return null;
}
protected String getAIString() { return "<snippet 386>";}
protected String[] getErrorFiles() { return new String[] {"<snippet 386>", };}

protected int[] getErrorFilesID() { return new int[] {386, };}

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
