import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_5539 extends AI {
public AI_5539() throws LeekRunException {
super(3, 1);
}
public void staticInit() throws LeekRunException {
}
public Object runIA(Session session) throws LeekRunException {
var u_s1 = new Box<Object>(AI_5539.this, new SetLeekValue(AI_5539.this, new Object[] { 1l, 2l, 3l }), 6);
var u_s2 = new Box<Object>(AI_5539.this, new SetLeekValue(AI_5539.this, new Object[] { 2l, 3l, 4l }), 6);
return Set_setDifference_hh(load(u_s1.get()), load(u_s2.get()));
}
protected String getAIString() { return "<snippet 5539>";}
protected String[] getErrorFiles() { return new String[] {"<snippet 5539>", };}

protected int[] getErrorFilesID() { return new int[] {5539, };}

private SetLeekValue Set_setDifference_hh(Object a0, Object a1) throws LeekRunException {
SetLeekValue x0; try { x0 = (SetLeekValue) (a0); } catch (ClassCastException e) { return new SetLeekValue(AI_5539.this); }
SetLeekValue x1; try { x1 = (SetLeekValue) (a1); } catch (ClassCastException e) { return new SetLeekValue(AI_5539.this); }
return x0.setDifference(this, x1);
}

}
